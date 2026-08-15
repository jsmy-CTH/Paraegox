//! RuntimeHost owns the bounded CoreService set and optional Deck run on one Node.

mod deck;
mod host;
mod status;

pub use deck::{CardContext, CardImplementation, DeckLaunch, DeckLaunchError};
pub use host::{
    CoreService, LifecycleAction, LifecycleFailure, LifecycleFailureKind, LifecycleOwner,
    MAX_CORE_SERVICES, RuntimeHost, RuntimeHostBuildError, RuntimeHostError,
};
pub use status::{
    CardInstanceId, CardInstanceSnapshot, CardInstanceState, DeckRunId, DeckRunSnapshot,
    DeckRunState, RuntimeHostIdentity, RuntimeHostSnapshot, RuntimeHostState, RuntimeStatusReader,
};

pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[cfg(test)]
mod tests {
    use std::future;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use paraegox_deck::{Card, CardDefinitionRef, CardKey, DeckCompiler, DeckKey, DeckSpec};
    use paraegox_kernel::RuntimeHostId;

    use super::{
        BoxError, CardContext, CardImplementation, CardInstanceState, CoreService, DeckLaunch,
        DeckLaunchError, DeckRunState, LifecycleFailureKind, LifecycleOwner, RuntimeHost,
        RuntimeHostError, RuntimeHostIdentity, RuntimeHostState,
    };

    type Events = Arc<Mutex<Vec<String>>>;

    struct RecordingService {
        label: &'static str,
        events: Events,
        fail_on_stop: bool,
        hang_on_stop: bool,
    }

    #[async_trait]
    impl CoreService for RecordingService {
        async fn start(&mut self, runtime: super::RuntimeStatusReader) -> Result<(), BoxError> {
            if runtime.snapshot().state != RuntimeHostState::Starting {
                return Err(io::Error::other("service did not observe Starting").into());
            }
            record(&self.events, format!("{}:start", self.label));
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), BoxError> {
            record(&self.events, format!("{}:stop", self.label));
            if self.hang_on_stop {
                future::pending::<()>().await;
            }
            if self.fail_on_stop {
                return Err(io::Error::other(format!("{} stop failed", self.label)).into());
            }
            Ok(())
        }
    }

    struct RecordingCard {
        key: CardKey,
        definition: CardDefinitionRef,
        label: &'static str,
        events: Events,
        fail_on_start: bool,
    }

    #[async_trait]
    impl CardImplementation for RecordingCard {
        fn card_key(&self) -> &CardKey {
            &self.key
        }

        fn definition(&self) -> &CardDefinitionRef {
            &self.definition
        }

        async fn start(&mut self, context: CardContext) -> Result<(), BoxError> {
            if context.card_key != self.key || context.definition != self.definition {
                return Err(io::Error::other("Card received the wrong start context").into());
            }
            record(&self.events, format!("{}:start", self.label));
            if self.fail_on_start {
                return Err(io::Error::other(format!("{} start failed", self.label)).into());
            }
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), BoxError> {
            record(&self.events, format!("{}:stop", self.label));
            Ok(())
        }
    }

    #[test]
    fn deck_launch_rejects_an_identity_mismatch_before_runtime_start() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lock = deck_lock(&[("agent", "builtin.agent@1")]);
        let implementation = RecordingCard {
            key: CardKey::new("wrong-key"),
            definition: CardDefinitionRef::new("builtin.agent@1"),
            label: "wrong",
            events: Arc::clone(&events),
            fail_on_start: false,
        };

        let result = DeckLaunch::new(lock, vec![Box::new(implementation)]);
        assert!(matches!(
            result,
            Err(DeckLaunchError::ImplementationMismatch { index: 0, .. })
        ));
        assert_events(&events, &[]);
    }

    #[tokio::test]
    async fn runtime_orders_multiple_services_before_cards_and_stops_in_reverse() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lock = deck_lock(&[
            ("agent", "builtin.agent@1"),
            ("terminal", "builtin.terminal@1"),
        ]);
        let launch = DeckLaunch::new(
            lock,
            vec![
                card("agent", "builtin.agent@1", "card-agent", &events, false),
                card(
                    "terminal",
                    "builtin.terminal@1",
                    "card-terminal",
                    &events,
                    false,
                ),
            ],
        )
        .expect("matching DeckLaunch");
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity(),
            vec![
                service("service-a", &events, false, false),
                service("service-b", &events, false, false),
            ],
            launch,
        )
        .expect("bounded RuntimeHost");

        runtime.start().await.expect("runtime should start");
        let ready = runtime.snapshot();
        let ready_deck = ready.deck_run.expect("Deck run should be observable");
        assert_eq!(ready.state, RuntimeHostState::Ready);
        assert_eq!(ready_deck.generation, 1);
        assert_eq!(ready_deck.state, DeckRunState::Ready);
        assert!(
            ready_deck
                .cards
                .iter()
                .all(|card| { card.generation == 1 && card.state == CardInstanceState::Ready })
        );
        assert_ne!(
            ready_deck.cards[0].card_instance_id,
            ready_deck.cards[1].card_instance_id
        );

        runtime.stop().await.expect("runtime should stop");
        let stopped = runtime.snapshot();
        let stopped_deck = stopped.deck_run.expect("Deck run remains observable");
        assert_eq!(stopped.state, RuntimeHostState::Stopped);
        assert_eq!(stopped_deck.deck_run_id, ready_deck.deck_run_id);
        assert_eq!(stopped_deck.state, DeckRunState::Stopped);
        assert!(
            stopped_deck
                .cards
                .iter()
                .all(|card| card.state == CardInstanceState::Stopped)
        );
        assert_events(
            &events,
            &[
                "service-a:start",
                "service-b:start",
                "card-agent:start",
                "card-terminal:start",
                "card-terminal:stop",
                "card-agent:stop",
                "service-b:stop",
                "service-a:stop",
            ],
        );
    }

    #[tokio::test]
    async fn card_start_failure_rolls_back_cards_then_services_and_collects_cleanup_errors() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lock = deck_lock(&[("first", "builtin.first@1"), ("second", "builtin.second@1")]);
        let launch = DeckLaunch::new(
            lock,
            vec![
                card("first", "builtin.first@1", "card-first", &events, false),
                card("second", "builtin.second@1", "card-second", &events, true),
            ],
        )
        .expect("matching DeckLaunch");
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity(),
            vec![
                service("service-a", &events, false, false),
                service("service-b", &events, true, false),
            ],
            launch,
        )
        .expect("bounded RuntimeHost");

        let error = runtime.start().await.expect_err("second Card must fail");
        let RuntimeHostError::StartFailed {
            failure,
            cleanup_failures,
        } = error
        else {
            panic!("unexpected RuntimeHost error")
        };
        assert_eq!(
            failure.owner,
            LifecycleOwner::Card {
                key: CardKey::new("second")
            }
        );
        assert!(matches!(failure.kind, LifecycleFailureKind::Owner(_)));
        assert_eq!(cleanup_failures.len(), 1);
        assert_eq!(
            cleanup_failures[0].owner,
            LifecycleOwner::CoreService { index: 1 }
        );

        let failed = runtime.snapshot();
        let failed_deck = failed.deck_run.expect("failed Deck remains observable");
        assert_eq!(failed.state, RuntimeHostState::Failed);
        assert_eq!(failed_deck.state, DeckRunState::Failed);
        assert_eq!(failed_deck.cards[0].state, CardInstanceState::Stopped);
        assert_eq!(failed_deck.cards[1].state, CardInstanceState::Failed);
        assert_events(
            &events,
            &[
                "service-a:start",
                "service-b:start",
                "card-first:start",
                "card-second:start",
                "card-first:stop",
                "service-b:stop",
                "service-a:stop",
            ],
        );
    }

    #[tokio::test]
    async fn a_hung_owner_lifecycle_is_bounded_by_the_runtime_deadline() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = RuntimeHost::with_lifecycle_timeout(
            runtime_identity(),
            vec![service("hung", &events, false, true)],
            Duration::from_millis(20),
        )
        .expect("bounded RuntimeHost");
        runtime.start().await.expect("runtime should start");

        let error = tokio::time::timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("RuntimeHost must enforce its own deadline")
            .expect_err("hung stop must fail");
        let RuntimeHostError::StopFailed { failures } = error else {
            panic!("unexpected RuntimeHost error")
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0].kind,
            LifecycleFailureKind::TimedOut { .. }
        ));
        assert_eq!(runtime.snapshot().state, RuntimeHostState::Failed);
        assert_events(&events, &["hung:start", "hung:stop"]);
    }

    fn runtime_identity() -> RuntimeHostIdentity {
        RuntimeHostIdentity::new(RuntimeHostId::new("runtime-test").expect("valid RuntimeHost id"))
    }

    fn service(
        label: &'static str,
        events: &Events,
        fail_on_stop: bool,
        hang_on_stop: bool,
    ) -> Box<dyn CoreService> {
        Box::new(RecordingService {
            label,
            events: Arc::clone(events),
            fail_on_stop,
            hang_on_stop,
        })
    }

    fn card(
        key: &str,
        definition: &str,
        label: &'static str,
        events: &Events,
        fail_on_start: bool,
    ) -> Box<dyn CardImplementation> {
        Box::new(RecordingCard {
            key: CardKey::new(key),
            definition: CardDefinitionRef::new(definition),
            label,
            events: Arc::clone(events),
            fail_on_start,
        })
    }

    fn deck_lock(cards: &[(&str, &str)]) -> paraegox_deck::DeckLock {
        let definitions = cards
            .iter()
            .map(|(_, definition)| CardDefinitionRef::new(*definition));
        let compiler = DeckCompiler::new(definitions).expect("valid definitions");
        compiler
            .compile(&DeckSpec {
                key: DeckKey::new("test-deck"),
                cards: cards
                    .iter()
                    .map(|(key, definition)| Card {
                        key: CardKey::new(*key),
                        definition: CardDefinitionRef::new(*definition),
                    })
                    .collect(),
            })
            .expect("valid Deck")
    }

    fn record(events: &Events, event: String) {
        events.lock().expect("events lock").push(event);
    }

    fn assert_events(events: &Events, expected: &[&str]) {
        let actual = events.lock().expect("events lock");
        assert_eq!(
            actual.iter().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
    }
}
