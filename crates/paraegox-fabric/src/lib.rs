//! Minimal Fabric CoreService: bounded exact query bindings over Zenoh.

mod client;
mod service;

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use tokio::time::Instant;
use zenoh::Config;
use zenoh::key_expr::KeyExpr;

pub use client::{FabricClient, query_one};
pub use service::{FabricHandle, FabricQueryBinding, FabricService};

const INGRESS_CAPACITY: usize = 16;
const MAX_QUERY_BINDINGS: usize = 8;
const MAX_IN_FLIGHT_REQUESTS: usize = 8;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const SERVICE_OPERATION_TIMEOUT: Duration = Duration::from_secs(3);

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
