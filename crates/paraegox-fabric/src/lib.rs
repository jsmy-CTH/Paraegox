//! Minimal Fabric CoreService: bounded exact query bindings over Zenoh.

use std::collections::HashSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use paraegox_runtime::{
    BoxError, CoreService, RuntimeHostSnapshot, RuntimeHostState, RuntimeStatusReader,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, timeout, timeout_at};
use zenoh::key_expr::KeyExpr;
use zenoh::query::{ConsolidationMode, Query, QueryTarget, Queryable};
use zenoh::{Config, Session, Wait};

const INGRESS_CAPACITY: usize = 16;
const MAX_QUERY_BINDINGS: usize = 8;
const MAX_IN_FLIGHT_REQUESTS: usize = 8;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const SERVICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(3);

pub struct FabricService {
    listen_endpoint: String,
    connect_endpoint: Option<String>,
    bindings: Option<Vec<FabricQueryBinding>>,
    session: Option<Arc<Session>>,
    queryables: Vec<Queryable<()>>,
    dispatcher_task: Option<JoinHandle<()>>,
    admission_open: Arc<AtomicBool>,
    handle_state: Arc<RwLock<FabricHandleState>>,
}

#[derive(Clone)]
pub struct FabricHandle {
    state: Arc<RwLock<FabricHandleState>>,
}

enum FabricHandleState {
    NotStarted,
    Running(Weak<Session>),
    Stopping,
    Stopped,
}

type QueryHandlerFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'static>>;
type QueryHandler =
    Arc<dyn Fn(RuntimeHostSnapshot, Vec<u8>) -> QueryHandlerFuture + Send + Sync + 'static>;

pub struct FabricQueryBinding {
    key: String,
    handler: QueryHandler,
}

pub struct FabricClient {
    session: Option<Session>,
}

struct AdmittedQuery {
    binding_index: usize,
    query: Query,
}

impl FabricQueryBinding {
    pub fn new<Handler, HandlerFuture>(
        key: impl Into<String>,
        handler: Handler,
    ) -> Result<Self, FabricError>
    where
        Handler: Fn(RuntimeHostSnapshot, Vec<u8>) -> HandlerFuture + Send + Sync + 'static,
        HandlerFuture: Future<Output = Result<Vec<u8>, String>> + Send + 'static,
    {
        let key = key.into();
        validate_exact_binding_key(&key)?;
        Ok(Self {
            key,
            handler: Arc::new(move |runtime, payload| Box::pin(handler(runtime, payload))),
        })
    }
}

impl FabricService {
    pub fn new<Encode>(
        listen_endpoint: impl Into<String>,
        binding_key: impl Into<String>,
        encode: Encode,
    ) -> Result<Self, FabricError>
    where
        Encode: Fn(RuntimeHostSnapshot) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    {
        Self::new_with_connect_endpoint(listen_endpoint, None, binding_key, encode)
    }

    pub fn new_with_connect_endpoint<Encode>(
        listen_endpoint: impl Into<String>,
        connect_endpoint: Option<String>,
        binding_key: impl Into<String>,
        encode: Encode,
    ) -> Result<Self, FabricError>
    where
        Encode: Fn(RuntimeHostSnapshot) -> Result<Vec<u8>, String> + Send + Sync + 'static,
    {
        let binding = FabricQueryBinding::new(binding_key, move |runtime, _payload| {
            let encoded = encode(runtime);
            async move { encoded }
        })?;
        Self::new_with_bindings(listen_endpoint, connect_endpoint, vec![binding])
    }

    pub fn new_with_bindings(
        listen_endpoint: impl Into<String>,
        connect_endpoint: Option<String>,
        bindings: Vec<FabricQueryBinding>,
    ) -> Result<Self, FabricError> {
        let listen_endpoint = listen_endpoint.into();
        validate_loopback_endpoint(&listen_endpoint)?;
        if let Some(connect_endpoint) = connect_endpoint.as_deref() {
            validate_loopback_endpoint(connect_endpoint)?;
        }
        if bindings.is_empty() {
            return Err(FabricError::new(
                "FabricService requires at least one exact query binding",
            ));
        }
        if bindings.len() > MAX_QUERY_BINDINGS {
            return Err(FabricError::new(format!(
                "FabricService accepts at most {MAX_QUERY_BINDINGS} exact query bindings"
            )));
        }
        let mut binding_keys = HashSet::with_capacity(bindings.len());
        for binding in &bindings {
            validate_exact_binding_key(&binding.key)?;
            if !binding_keys.insert(binding.key.as_str()) {
                return Err(FabricError::new(format!(
                    "duplicate Fabric query binding: {}",
                    binding.key
                )));
            }
        }

        Ok(Self {
            listen_endpoint,
            connect_endpoint,
            bindings: Some(bindings),
            session: None,
            queryables: Vec::new(),
            dispatcher_task: None,
            admission_open: Arc::new(AtomicBool::new(false)),
            handle_state: Arc::new(RwLock::new(FabricHandleState::NotStarted)),
        })
    }

    pub fn handle(&self) -> FabricHandle {
        FabricHandle {
            state: Arc::clone(&self.handle_state),
        }
    }

    async fn start_inner(&mut self, runtime: RuntimeStatusReader) -> Result<(), FabricError> {
        if self.session.is_some() {
            return Err(FabricError::new("FabricService is already started"));
        }
        let bindings = self
            .bindings
            .take()
            .ok_or_else(|| FabricError::new("Fabric query bindings are unavailable"))?;

        let config = node_config(&self.listen_endpoint, self.connect_endpoint.as_deref())?;
        let session = timeout(SERVICE_OPERATION_TIMEOUT, zenoh::open(config))
            .await
            .map_err(|_| FabricError::new("timed out opening the Fabric session"))?
            .map_err(|error| FabricError::context("could not open the Fabric session", error))?;

        let (sender, receiver) = mpsc::channel(INGRESS_CAPACITY);
        let mut queryables = Vec::with_capacity(bindings.len());
        for (binding_index, binding) in bindings.iter().enumerate() {
            let binding_sender = sender.clone();
            let admission_open = Arc::clone(&self.admission_open);
            let queryable_result = timeout(
                SERVICE_OPERATION_TIMEOUT,
                session
                    .declare_queryable(binding.key.clone())
                    .complete(true)
                    .callback(move |query| {
                        admit_query(&binding_sender, &admission_open, binding_index, query);
                    }),
            )
            .await;

            match queryable_result {
                Ok(Ok(queryable)) => queryables.push(queryable),
                Ok(Err(error)) => {
                    drop(sender);
                    undeclare_queryables(&mut queryables).await;
                    close_session(&session).await;
                    return Err(FabricError::context(
                        "could not declare a Fabric query binding",
                        error,
                    ));
                }
                Err(_) => {
                    drop(sender);
                    undeclare_queryables(&mut queryables).await;
                    close_session(&session).await;
                    return Err(FabricError::new(
                        "timed out declaring a Fabric query binding",
                    ));
                }
            }
        }
        drop(sender);

        let dispatcher_task = tokio::spawn(dispatch_requests(receiver, bindings, runtime));
        self.session = Some(Arc::new(session));
        self.queryables = queryables;
        self.dispatcher_task = Some(dispatcher_task);
        self.admission_open.store(true, Ordering::Release);

        if let Err(publish_error) = self.publish_session() {
            return match self.stop_inner().await {
                Ok(()) => Err(publish_error),
                Err(cleanup_error) => Err(FabricError::new(format!(
                    "{publish_error}; cleanup after publish failure also failed: {cleanup_error}"
                ))),
            };
        }
        Ok(())
    }

    async fn stop_inner(&mut self) -> Result<(), FabricError> {
        let mut cleanup_errors = Vec::new();
        self.admission_open.store(false, Ordering::Release);

        if let Err(error) = self.begin_stop() {
            cleanup_errors.push(error.to_string());
        }

        let cleanup_deadline = Instant::now() + SERVICE_OPERATION_TIMEOUT;
        for queryable in self.queryables.drain(..) {
            match timeout_at(cleanup_deadline, queryable.undeclare()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    cleanup_errors.push(format!("could not undeclare Fabric binding: {error}"));
                }
                Err(_) => {
                    cleanup_errors.push("timed out undeclaring the Fabric binding".to_owned());
                }
            }
        }

        if let Some(mut task) = self.dispatcher_task.take() {
            match timeout_at(cleanup_deadline, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    cleanup_errors.push(format!("Fabric dispatcher task failed: {error}"));
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    cleanup_errors.push("timed out joining the Fabric dispatcher task".to_owned());
                }
            }
        }

        if let Some(session) = self.session.take() {
            match timeout(SERVICE_OPERATION_TIMEOUT, session.close()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => cleanup_errors.push(format!("could not close Fabric: {error}")),
                Err(_) => cleanup_errors.push("timed out closing Fabric".to_owned()),
            }
        }

        if let Err(error) = self.finish_stop() {
            cleanup_errors.push(error.to_string());
        }

        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(FabricError::new(cleanup_errors.join("; ")))
        }
    }

    fn publish_session(&self) -> Result<(), FabricError> {
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| FabricError::new("Fabric session is unavailable during start"))?;
        let mut state = self.handle_state.write().map_err(|_| {
            FabricError::new("Fabric handle state lock is poisoned while publishing the session")
        })?;
        *state = FabricHandleState::Running(Arc::downgrade(session));
        Ok(())
    }

    fn begin_stop(&self) -> Result<(), FabricError> {
        match self.handle_state.write() {
            Ok(mut state) => {
                *state = FabricHandleState::Stopping;
                Ok(())
            }
            Err(poisoned) => {
                *poisoned.into_inner() = FabricHandleState::Stopping;
                Err(FabricError::new(
                    "Fabric handle state lock is poisoned while stopping; the published session was cleared",
                ))
            }
        }
    }

    fn finish_stop(&self) -> Result<(), FabricError> {
        match self.handle_state.write() {
            Ok(mut state) => {
                *state = FabricHandleState::Stopped;
                Ok(())
            }
            Err(poisoned) => {
                *poisoned.into_inner() = FabricHandleState::Stopped;
                Err(FabricError::new(
                    "Fabric handle state lock is poisoned after stopping",
                ))
            }
        }
    }
}

impl Drop for FabricService {
    fn drop(&mut self) {
        self.admission_open.store(false, Ordering::Release);
        if let Some(task) = self.dispatcher_task.take() {
            task.abort();
        }
        match self.handle_state.write() {
            Ok(mut state) => *state = FabricHandleState::Stopped,
            Err(poisoned) => *poisoned.into_inner() = FabricHandleState::Stopped,
        }
    }
}

impl FabricHandle {
    async fn request_one(
        &self,
        binding_key: &str,
        payload: &[u8],
        deadline: Duration,
    ) -> Result<Vec<u8>, FabricError> {
        validate_request(binding_key, payload, deadline)?;
        let expires_at = query_deadline(deadline)?;
        let session = self.running_session()?;

        match request_with_session(&session, binding_key, payload, expires_at).await {
            Ok(payload) => Ok(payload),
            Err(request_error) => match self.state.read() {
                Ok(state) if matches!(*state, FabricHandleState::Running(_)) => Err(request_error),
                Ok(_) => Err(FabricError::context(
                    "FabricService stopped while the request was in flight",
                    request_error,
                )),
                Err(_) => Err(FabricError::new(format!(
                    "Fabric handle state lock is poisoned after a request failure: {request_error}"
                ))),
            },
        }
    }

    pub async fn query_one(
        &self,
        binding_key: &str,
        deadline: Duration,
    ) -> Result<Vec<u8>, FabricError> {
        self.request_one(binding_key, &[], deadline).await
    }

    fn running_session(&self) -> Result<Arc<Session>, FabricError> {
        let state = self.state.read().map_err(|_| {
            FabricError::new("Fabric handle state lock is poisoned while starting a query")
        })?;
        match &*state {
            FabricHandleState::Running(session) => session.upgrade().ok_or_else(|| {
                FabricError::new("FabricService session owner is no longer available")
            }),
            FabricHandleState::NotStarted => Err(FabricError::new(
                "FabricService has not published its session yet",
            )),
            FabricHandleState::Stopping => Err(FabricError::new("FabricService is stopping")),
            FabricHandleState::Stopped => Err(FabricError::new("FabricService is stopped")),
        }
    }
}

impl FabricClient {
    pub async fn connect(connect_endpoint: &str, deadline: Duration) -> Result<Self, FabricError> {
        validate_loopback_endpoint(connect_endpoint)?;
        validate_deadline(deadline, "Fabric client connect")?;

        let expires_at = query_deadline(deadline)?;
        let config = probe_config(connect_endpoint)?;
        let session = timeout_at(expires_at, zenoh::open(config))
            .await
            .map_err(|_| FabricError::new("Fabric client timed out while connecting"))?
            .map_err(|error| FabricError::context("could not connect the Fabric client", error))?;

        Ok(Self {
            session: Some(session),
        })
    }

    pub async fn request_one(
        &self,
        binding_key: &str,
        payload: &[u8],
        deadline: Duration,
    ) -> Result<Vec<u8>, FabricError> {
        validate_request(binding_key, payload, deadline)?;
        let expires_at = query_deadline(deadline)?;
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| FabricError::new("Fabric client is closed"))?;
        request_with_session(session, binding_key, payload, expires_at).await
    }

    pub async fn close(&mut self, deadline: Duration) -> Result<(), FabricError> {
        validate_deadline(deadline, "Fabric client close")?;
        let expires_at = query_deadline(deadline)?;
        let session = self
            .session
            .take()
            .ok_or_else(|| FabricError::new("Fabric client is already closed"))?;

        timeout_at(expires_at, session.close())
            .await
            .map_err(|_| FabricError::new("Fabric client timed out while closing"))?
            .map_err(|error| FabricError::context("could not close the Fabric client", error))
    }
}

#[async_trait]
impl CoreService for FabricService {
    async fn start(&mut self, runtime: RuntimeStatusReader) -> Result<(), BoxError> {
        self.start_inner(runtime)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }

    async fn stop(&mut self) -> Result<(), BoxError> {
        self.stop_inner()
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

pub async fn query_one(
    connect_endpoint: &str,
    binding_key: &str,
    deadline: Duration,
) -> Result<Vec<u8>, FabricError> {
    validate_loopback_endpoint(connect_endpoint)?;
    validate_request(binding_key, &[], deadline)?;

    let expires_at = query_deadline(deadline)?;
    let config = probe_config(connect_endpoint)?;
    let session = timeout_at(expires_at, zenoh::open(config))
        .await
        .map_err(|_| FabricError::new("query timed out while connecting to Fabric"))?
        .map_err(|error| FabricError::context("could not connect to Fabric", error))?;

    let result = request_with_session(&session, binding_key, &[], expires_at).await;
    let close_result = timeout_at(expires_at, session.close()).await;

    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(_)) => Err(FabricError::new(
            "query timed out while closing its Fabric session",
        )),
        (Ok(_), Ok(Err(error))) => Err(FabricError::context(
            "could not close the probe Fabric session",
            error,
        )),
        (Ok(status), Ok(Ok(()))) => Ok(status),
    }
}

async fn request_with_session(
    session: &Session,
    binding_key: &str,
    payload: &[u8],
    expires_at: Instant,
) -> Result<Vec<u8>, FabricError> {
    let remaining = expires_at.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(FabricError::new("query deadline elapsed before send"));
    }

    let request = session
        .get(binding_key)
        .target(QueryTarget::AllComplete)
        .consolidation(ConsolidationMode::None)
        .timeout(remaining);
    let request = if payload.is_empty() {
        request
    } else {
        request.payload(payload.to_vec())
    };
    let replies = timeout_at(expires_at, request)
        .await
        .map_err(|_| FabricError::new("query timed out while sending to Fabric"))?
        .map_err(|error| FabricError::context("could not send the Fabric query", error))?;

    let reply = timeout_at(expires_at, replies.recv_async())
        .await
        .map_err(|_| FabricError::new("query timed out waiting for a response"))?
        .map_err(|_| FabricError::new("Fabric returned no response before the deadline"))?;
    let sample = reply.result().map_err(|error| {
        let message = error
            .payload()
            .try_to_string()
            .map_or_else(|_| error.to_string(), |payload| payload.into_owned());
        FabricError::new(format!(
            "remote Fabric binding rejected the query: {message}"
        ))
    })?;
    let payload = sample.payload();
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(FabricError::new("Fabric response exceeds 64 KiB"));
    }

    Ok(payload.to_bytes().into_owned())
}

fn admit_query(
    sender: &mpsc::Sender<AdmittedQuery>,
    admission_open: &AtomicBool,
    binding_index: usize,
    query: Query,
) {
    if !admission_open.load(Ordering::Acquire) {
        reply_error_now(query, "FabricService is stopping");
        return;
    }

    if query
        .payload()
        .is_some_and(|payload| payload.len() > MAX_PAYLOAD_BYTES)
    {
        reply_error_now(query, "Fabric request exceeds 64 KiB");
        return;
    }

    let admitted = AdmittedQuery {
        binding_index,
        query,
    };
    match sender.try_send(admitted) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(admitted)) => {
            reply_error_now(admitted.query, "FabricService is busy")
        }
        Err(mpsc::error::TrySendError::Closed(admitted)) => {
            reply_error_now(admitted.query, "FabricService is stopping")
        }
    }
}

async fn dispatch_requests(
    mut receiver: mpsc::Receiver<AdmittedQuery>,
    bindings: Vec<FabricQueryBinding>,
    runtime: RuntimeStatusReader,
) {
    let mut in_flight = JoinSet::new();

    loop {
        while in_flight.len() >= MAX_IN_FLIGHT_REQUESTS {
            let _ = in_flight.join_next().await;
        }

        let Some(admitted) = receiver.recv().await else {
            break;
        };
        let handler = Arc::clone(&bindings[admitted.binding_index].handler);
        let runtime_snapshot = runtime.snapshot();
        in_flight.spawn(serve_request(admitted.query, handler, runtime_snapshot));
    }

    while in_flight.join_next().await.is_some() {}
}

async fn serve_request(query: Query, handler: QueryHandler, runtime_snapshot: RuntimeHostSnapshot) {
    if runtime_snapshot.state != RuntimeHostState::Ready {
        let _ = timeout(
            SERVICE_OPERATION_TIMEOUT,
            query.reply_err("RuntimeHost is not ready"),
        )
        .await;
        return;
    }

    let request_payload = query
        .payload()
        .map(|payload| payload.to_bytes().into_owned())
        .unwrap_or_default();

    match handler(runtime_snapshot, request_payload).await {
        Ok(payload) if payload.len() <= MAX_PAYLOAD_BYTES => {
            let key = query.key_expr().clone();
            let _ = timeout(SERVICE_OPERATION_TIMEOUT, query.reply(key, payload)).await;
        }
        Ok(_) => {
            let _ = timeout(
                SERVICE_OPERATION_TIMEOUT,
                query.reply_err("Fabric response exceeds 64 KiB"),
            )
            .await;
        }
        Err(error) if error.len() <= MAX_PAYLOAD_BYTES => {
            let _ = timeout(SERVICE_OPERATION_TIMEOUT, query.reply_err(error)).await;
        }
        Err(_) => {
            let _ = timeout(
                SERVICE_OPERATION_TIMEOUT,
                query.reply_err("Fabric handler error exceeds 64 KiB"),
            )
            .await;
        }
    }
}

fn reply_error_now(query: Query, message: &'static str) {
    let _ = query.reply_err(message).wait();
}

fn node_config(
    listen_endpoint: &str,
    connect_endpoint: Option<&str>,
) -> Result<Config, FabricError> {
    let mut config = base_config("peer")?;
    insert_endpoints(&mut config, "listen/endpoints", listen_endpoint)?;
    if let Some(connect_endpoint) = connect_endpoint {
        insert_endpoints(&mut config, "connect/endpoints", connect_endpoint)?;
    }
    Ok(config)
}

fn probe_config(connect_endpoint: &str) -> Result<Config, FabricError> {
    let mut config = base_config("client")?;
    insert_endpoints(&mut config, "connect/endpoints", connect_endpoint)?;
    Ok(config)
}

fn base_config(mode: &str) -> Result<Config, FabricError> {
    let mut config = Config::default();
    let mode = serde_json::to_string(mode)
        .map_err(|error| FabricError::context("could not encode Fabric mode", error))?;
    config
        .insert_json5("mode", &mode)
        .map_err(|error| FabricError::context("could not configure Fabric mode", error))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| FabricError::context("could not disable multicast scouting", error))?;
    config
        .insert_json5("transport/link/rx/max_message_size", "65536")
        .map_err(|error| FabricError::context("could not bound Fabric messages", error))?;
    Ok(config)
}

fn insert_endpoints(config: &mut Config, key: &str, endpoint: &str) -> Result<(), FabricError> {
    let endpoints = serde_json::to_string(&[endpoint])
        .map_err(|error| FabricError::context("could not encode Fabric endpoint", error))?;
    config
        .insert_json5(key, &endpoints)
        .map_err(|error| FabricError::context("could not configure Fabric endpoint", error))
}

fn validate_loopback_endpoint(endpoint: &str) -> Result<(), FabricError> {
    let port = endpoint
        .strip_prefix("tcp/127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok());
    if port.is_some_and(|port| port != 0) {
        return Ok(());
    }
    Err(FabricError::new(
        "this baseline only accepts tcp/127.0.0.1:<port>; authenticated remote Fabric is not implemented",
    ))
}

fn validate_exact_binding_key(binding_key: &str) -> Result<(), FabricError> {
    if binding_key.is_empty() {
        return Err(FabricError::new("Fabric binding key must not be empty"));
    }
    KeyExpr::new(binding_key)
        .map_err(|error| FabricError::context("Fabric binding key is invalid", error))?;
    if binding_key.contains('*') {
        return Err(FabricError::new(
            "Fabric query bindings must use an exact key without wildcards",
        ));
    }
    Ok(())
}

fn validate_request(
    binding_key: &str,
    payload: &[u8],
    deadline: Duration,
) -> Result<(), FabricError> {
    validate_exact_binding_key(binding_key)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(FabricError::new("Fabric request exceeds 64 KiB"));
    }
    if deadline.is_zero() {
        return Err(FabricError::new("query deadline must be greater than zero"));
    }
    Ok(())
}

fn validate_deadline(deadline: Duration, operation: &str) -> Result<(), FabricError> {
    if deadline.is_zero() {
        return Err(FabricError::new(format!(
            "{operation} deadline must be greater than zero"
        )));
    }
    Ok(())
}

fn query_deadline(deadline: Duration) -> Result<Instant, FabricError> {
    Instant::now()
        .checked_add(deadline)
        .ok_or_else(|| FabricError::new("query deadline is too large"))
}

async fn close_session(session: &Session) {
    let _ = timeout(SERVICE_OPERATION_TIMEOUT, session.close()).await;
}

async fn undeclare_queryables(queryables: &mut Vec<Queryable<()>>) {
    let cleanup_deadline = Instant::now() + SERVICE_OPERATION_TIMEOUT;
    for queryable in queryables.drain(..) {
        let _ = timeout_at(cleanup_deadline, queryable.undeclare()).await;
    }
}

#[derive(Debug)]
pub struct FabricError {
    message: String,
}

impl FabricError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn context(context: &str, source: impl Display) -> Self {
        Self::new(format!("{context}: {source}"))
    }
}

impl Display for FabricError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for FabricError {}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::time::Duration;

    use paraegox_kernel::RuntimeHostId;
    use paraegox_runtime::{RuntimeHost, RuntimeHostIdentity};

    use super::FabricService;

    #[test]
    fn dropping_the_service_owner_revokes_its_handle() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test endpoint");
        let endpoint = format!(
            "tcp/127.0.0.1:{}",
            listener.local_addr().expect("test address").port()
        );
        drop(listener);

        let service =
            FabricService::new(&endpoint, "paraegox/v1/test/status", |_| Ok(b"{}".to_vec()))
                .expect("valid Fabric service");
        let handle = service.handle();
        let identity = RuntimeHostIdentity::new(
            RuntimeHostId::new("fabric-drop-test").expect("valid RuntimeHost id"),
        );
        let mut owner = RuntimeHost::new(identity, vec![Box::new(service)])
            .expect("bounded CoreService composition");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .expect("test runtime");

        runtime.block_on(async {
            owner.start().await.expect("Fabric owner should start");
            drop(owner);

            let error = handle
                .query_one("paraegox/v1/test/status", Duration::from_millis(100))
                .await
                .expect_err("a handle must stop admitting work after its owner is dropped");
            assert_eq!(error.to_string(), "FabricService is stopped");
        });
    }
}
