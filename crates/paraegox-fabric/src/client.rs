use std::time::Duration;

use tokio::time::{Instant, timeout_at};
use zenoh::Session;
use zenoh::query::{ConsolidationMode, QueryTarget};

use crate::{
    FabricError, MAX_PAYLOAD_BYTES, probe_config, query_deadline, validate_deadline,
    validate_loopback_endpoint, validate_request,
};

pub struct FabricClient {
    session: Option<Session>,
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

pub(crate) async fn request_with_session(
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
