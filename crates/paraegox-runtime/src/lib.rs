//! RuntimeHost owns the lifecycle of the CoreService set on one Node.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use paraegox_kernel::{RuntimeHostEpoch, RuntimeHostId};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;
const CORE_SERVICE_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostState {
    Created,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

impl RuntimeHostState {
    fn as_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(value: u8) -> Self {
        match value {
            value if value == Self::Created.as_u8() => Self::Created,
            value if value == Self::Starting.as_u8() => Self::Starting,
            value if value == Self::Ready.as_u8() => Self::Ready,
            value if value == Self::Stopping.as_u8() => Self::Stopping,
            value if value == Self::Stopped.as_u8() => Self::Stopped,
            _ => Self::Failed,
        }
    }
}

impl Display for RuntimeHostState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHostIdentity {
    pub runtime_host_id: RuntimeHostId,
    pub epoch: RuntimeHostEpoch,
}

impl RuntimeHostIdentity {
    pub fn new(runtime_host_id: RuntimeHostId) -> Self {
        Self {
            runtime_host_id,
            epoch: RuntimeHostEpoch::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHostSnapshot {
    pub identity: RuntimeHostIdentity,
    pub state: RuntimeHostState,
}

#[derive(Clone)]
pub struct RuntimeStatusReader {
    identity: RuntimeHostIdentity,
    state: Arc<AtomicU8>,
}

impl RuntimeStatusReader {
    pub fn snapshot(&self) -> RuntimeHostSnapshot {
        RuntimeHostSnapshot {
            identity: self.identity.clone(),
            state: RuntimeHostState::from_u8(self.state.load(Ordering::Acquire)),
        }
    }
}

/// A CoreService must clean up any partially started resources before returning
/// an error from `start`. `stop` must not return until its owned work is joined.
#[async_trait]
pub trait CoreService: Send {
    async fn start(&mut self, runtime: RuntimeStatusReader) -> Result<(), BoxError>;

    async fn stop(&mut self) -> Result<(), BoxError>;
}

pub struct RuntimeHost<Service> {
    status: RuntimeStatusReader,
    service: Service,
    lifecycle_timeout: Duration,
}

impl<Service> RuntimeHost<Service>
where
    Service: CoreService,
{
    pub fn new(identity: RuntimeHostIdentity, service: Service) -> Self {
        Self {
            status: RuntimeStatusReader {
                identity,
                state: Arc::new(AtomicU8::new(RuntimeHostState::Created.as_u8())),
            },
            service,
            lifecycle_timeout: CORE_SERVICE_LIFECYCLE_TIMEOUT,
        }
    }

    pub fn snapshot(&self) -> RuntimeHostSnapshot {
        self.status.snapshot()
    }

    pub async fn start(&mut self) -> Result<(), RuntimeHostError> {
        self.require_state("start", RuntimeHostState::Created)?;
        self.set_state(RuntimeHostState::Starting);

        match timeout(
            self.lifecycle_timeout,
            self.service.start(self.status.clone()),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(source)) => {
                self.set_state(RuntimeHostState::Failed);
                return Err(RuntimeHostError::ServiceStart(source));
            }
            Err(_) => {
                self.set_state(RuntimeHostState::Failed);
                let _ = timeout(self.lifecycle_timeout, self.service.stop()).await;
                return Err(RuntimeHostError::LifecycleTimeout("start"));
            }
        }

        self.set_state(RuntimeHostState::Ready);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<(), RuntimeHostError> {
        self.require_state("stop", RuntimeHostState::Ready)?;
        self.set_state(RuntimeHostState::Stopping);

        match timeout(self.lifecycle_timeout, self.service.stop()).await {
            Ok(Ok(())) => {}
            Ok(Err(source)) => {
                self.set_state(RuntimeHostState::Failed);
                return Err(RuntimeHostError::ServiceStop(source));
            }
            Err(_) => {
                self.set_state(RuntimeHostState::Failed);
                return Err(RuntimeHostError::LifecycleTimeout("stop"));
            }
        }

        self.set_state(RuntimeHostState::Stopped);
        Ok(())
    }

    fn require_state(
        &self,
        action: &'static str,
        expected: RuntimeHostState,
    ) -> Result<(), RuntimeHostError> {
        let actual = self.snapshot().state;
        if actual == expected {
            Ok(())
        } else {
            Err(RuntimeHostError::InvalidTransition { action, actual })
        }
    }

    fn set_state(&self, state: RuntimeHostState) {
        self.status.state.store(state.as_u8(), Ordering::Release);
    }

    #[cfg(test)]
    fn with_lifecycle_timeout(
        identity: RuntimeHostIdentity,
        service: Service,
        lifecycle_timeout: Duration,
    ) -> Self {
        let mut runtime = Self::new(identity, service);
        runtime.lifecycle_timeout = lifecycle_timeout;
        runtime
    }
}

#[derive(Debug)]
pub enum RuntimeHostError {
    InvalidTransition {
        action: &'static str,
        actual: RuntimeHostState,
    },
    LifecycleTimeout(&'static str),
    ServiceStart(BoxError),
    ServiceStop(BoxError),
}

impl Display for RuntimeHostError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { action, actual } => {
                write!(
                    formatter,
                    "cannot {action} RuntimeHost while it is {actual}"
                )
            }
            Self::LifecycleTimeout(action) => {
                write!(
                    formatter,
                    "CoreService {action} exceeded its lifecycle deadline"
                )
            }
            Self::ServiceStart(source) => write!(formatter, "CoreService start failed: {source}"),
            Self::ServiceStop(source) => write!(formatter, "CoreService stop failed: {source}"),
        }
    }
}

impl Error for RuntimeHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidTransition { .. } | Self::LifecycleTimeout(_) => None,
            Self::ServiceStart(source) | Self::ServiceStop(source) => Some(source.as_ref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use paraegox_kernel::RuntimeHostId;

    use super::{BoxError, CoreService, RuntimeHost, RuntimeHostIdentity, RuntimeHostState};

    struct RecordingService {
        events: Arc<Mutex<Vec<&'static str>>>,
        hang_on_stop: bool,
    }

    #[async_trait]
    impl CoreService for RecordingService {
        async fn start(&mut self, runtime: super::RuntimeStatusReader) -> Result<(), BoxError> {
            if runtime.snapshot().state != RuntimeHostState::Starting {
                return Err(io::Error::other("service did not observe Starting").into());
            }
            self.events.lock().expect("events lock").push("start");
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), BoxError> {
            if self.hang_on_stop {
                future::pending::<()>().await;
            }
            self.events.lock().expect("events lock").push("stop");
            Ok(())
        }
    }

    #[tokio::test]
    async fn runtime_host_enforces_and_observes_its_lifecycle() {
        let runtime_host_id = RuntimeHostId::new("runtime-test").expect("valid runtime id");
        let events = Arc::new(Mutex::new(Vec::new()));
        let service = RecordingService {
            events: Arc::clone(&events),
            hang_on_stop: false,
        };
        let mut runtime =
            RuntimeHost::new(RuntimeHostIdentity::new(runtime_host_id.clone()), service);

        runtime.start().await.expect("runtime should start");
        assert_eq!(runtime.snapshot().state, RuntimeHostState::Ready);
        assert!(runtime.start().await.is_err(), "double start must fail");

        runtime.stop().await.expect("runtime should stop");
        assert_eq!(runtime.snapshot().state, RuntimeHostState::Stopped);
        assert!(runtime.stop().await.is_err(), "double stop must fail");
        assert_eq!(*events.lock().expect("events lock"), ["start", "stop"]);

        let hanging_service = RecordingService {
            events: Arc::new(Mutex::new(Vec::new())),
            hang_on_stop: true,
        };
        let mut bounded_runtime = RuntimeHost::with_lifecycle_timeout(
            RuntimeHostIdentity::new(runtime_host_id),
            hanging_service,
            Duration::from_millis(10),
        );
        bounded_runtime.start().await.expect("runtime should start");
        assert!(
            bounded_runtime.stop().await.is_err(),
            "a hung CoreService must not hang RuntimeHost"
        );
        assert_eq!(bounded_runtime.snapshot().state, RuntimeHostState::Failed);
    }
}
