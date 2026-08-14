//! Minimal Fabric CoreService: one bounded exact query binding over Zenoh.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use async_trait::async_trait;
use paraegox_runtime::{
    BoxError, CoreService, RuntimeHostSnapshot, RuntimeHostState, RuntimeStatusReader,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};
use zenoh::query::{ConsolidationMode, Query, QueryTarget, Queryable};
use zenoh::{Config, Session, Wait};

const INGRESS_CAPACITY: usize = 16;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const SERVICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(3);

pub struct FabricService {
    listen_endpoint: String,
    binding: Option<RuntimeStatusBinding>,
    session: Option<Session>,
    queryable: Option<Queryable<()>>,
    request_task: Option<JoinHandle<()>>,
}

type StatusEncoder =
    Box<dyn Fn(RuntimeHostSnapshot) -> Result<Vec<u8>, String> + Send + Sync + 'static>;

struct RuntimeStatusBinding {
    key: String,
    encode: StatusEncoder,
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
        let listen_endpoint = listen_endpoint.into();
        validate_loopback_endpoint(&listen_endpoint)?;
        let binding_key = binding_key.into();
        if binding_key.is_empty() {
            return Err(FabricError::new("Fabric binding key must not be empty"));
        }
        Ok(Self {
            listen_endpoint,
            binding: Some(RuntimeStatusBinding {
                key: binding_key,
                encode: Box::new(encode),
            }),
            session: None,
            queryable: None,
            request_task: None,
        })
    }

    async fn start_inner(&mut self, runtime: RuntimeStatusReader) -> Result<(), FabricError> {
        if self.session.is_some() {
            return Err(FabricError::new("FabricService is already started"));
        }
        let binding = self
            .binding
            .take()
            .ok_or_else(|| FabricError::new("Fabric status binding is unavailable"))?;

        let config = node_config(&self.listen_endpoint)?;
        let session = timeout(SERVICE_OPERATION_TIMEOUT, zenoh::open(config))
            .await
            .map_err(|_| FabricError::new("timed out opening the Fabric session"))?
            .map_err(|error| FabricError::context("could not open the Fabric session", error))?;

        let (sender, receiver) = mpsc::channel(INGRESS_CAPACITY);
        let queryable_result = timeout(
            SERVICE_OPERATION_TIMEOUT,
            session
                .declare_queryable(binding.key)
                .complete(true)
                .callback(move |query| admit_query(&sender, query)),
        )
        .await;

        let queryable = match queryable_result {
            Ok(Ok(queryable)) => queryable,
            Ok(Err(error)) => {
                close_session(&session).await;
                return Err(FabricError::context(
                    "could not declare the Fabric query binding",
                    error,
                ));
            }
            Err(_) => {
                close_session(&session).await;
                return Err(FabricError::new(
                    "timed out declaring the Fabric query binding",
                ));
            }
        };

        let request_task = tokio::spawn(serve_status_requests(receiver, binding.encode, runtime));
        self.session = Some(session);
        self.queryable = Some(queryable);
        self.request_task = Some(request_task);
        Ok(())
    }

    async fn stop_inner(&mut self) -> Result<(), FabricError> {
        let mut cleanup_errors = Vec::new();

        if let Some(queryable) = self.queryable.take() {
            match timeout(SERVICE_OPERATION_TIMEOUT, queryable.undeclare()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    cleanup_errors.push(format!("could not undeclare Fabric binding: {error}"));
                }
                Err(_) => {
                    cleanup_errors.push("timed out undeclaring the Fabric binding".to_owned());
                }
            }
        }

        if let Some(mut task) = self.request_task.take() {
            match timeout(SERVICE_OPERATION_TIMEOUT, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    cleanup_errors.push(format!("Fabric request task failed: {error}"));
                }
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    cleanup_errors.push("timed out joining the Fabric request task".to_owned());
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

        if cleanup_errors.is_empty() {
            Ok(())
        } else {
            Err(FabricError::new(cleanup_errors.join("; ")))
        }
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
    if binding_key.is_empty() {
        return Err(FabricError::new("Fabric binding key must not be empty"));
    }
    if deadline.is_zero() {
        return Err(FabricError::new("query deadline must be greater than zero"));
    }

    let expires_at = Instant::now() + deadline;
    let config = probe_config(connect_endpoint)?;
    let session = timeout_at(expires_at, zenoh::open(config))
        .await
        .map_err(|_| FabricError::new("query timed out while connecting to Fabric"))?
        .map_err(|error| FabricError::context("could not connect to Fabric", error))?;

    let result = query_with_session(&session, binding_key, expires_at).await;
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

async fn query_with_session(
    session: &Session,
    binding_key: &str,
    expires_at: Instant,
) -> Result<Vec<u8>, FabricError> {
    let remaining = expires_at.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(FabricError::new("query deadline elapsed before send"));
    }

    let replies = session
        .get(binding_key)
        .target(QueryTarget::AllComplete)
        .consolidation(ConsolidationMode::None)
        .timeout(remaining)
        .await
        .map_err(|error| FabricError::context("could not send the Node status query", error))?;

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

fn admit_query(sender: &mpsc::Sender<Query>, query: Query) {
    if query
        .payload()
        .is_some_and(|payload| payload.len() > MAX_PAYLOAD_BYTES)
    {
        reply_error_now(query, "Fabric request exceeds 64 KiB");
        return;
    }

    match sender.try_send(query) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(query)) => {
            reply_error_now(query, "FabricService is busy")
        }
        Err(mpsc::error::TrySendError::Closed(query)) => {
            reply_error_now(query, "FabricService is stopping")
        }
    }
}

async fn serve_status_requests(
    mut receiver: mpsc::Receiver<Query>,
    encode: StatusEncoder,
    runtime: RuntimeStatusReader,
) {
    while let Some(query) = receiver.recv().await {
        let runtime_snapshot = runtime.snapshot();
        if runtime_snapshot.state != RuntimeHostState::Ready {
            let _ = timeout(
                SERVICE_OPERATION_TIMEOUT,
                query.reply_err("RuntimeHost is not ready"),
            )
            .await;
            continue;
        }

        match encode(runtime_snapshot) {
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
            Err(error) => {
                let _ = timeout(SERVICE_OPERATION_TIMEOUT, query.reply_err(error)).await;
            }
        }
    }
}

fn reply_error_now(query: Query, message: &'static str) {
    let _ = query.reply_err(message).wait();
}

fn node_config(listen_endpoint: &str) -> Result<Config, FabricError> {
    let mut config = base_config("peer")?;
    insert_endpoints(&mut config, "listen/endpoints", listen_endpoint)?;
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

async fn close_session(session: &Session) {
    let _ = timeout(SERVICE_OPERATION_TIMEOUT, session.close()).await;
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
