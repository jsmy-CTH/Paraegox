//! Completion providers owned by AgentService.

use std::env;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Client, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use crate::{
    AgentError, BUILTIN_AGENT_DEFINITION, DEEPSEEK_V4_FLASH_AGENT_DEFINITION, TurnFailure,
};

pub(crate) const MAX_MODEL_CONTENT_BYTES: usize = 16 * 1024;
pub(crate) const MAX_PROVIDER_REQUEST_BYTES: usize = 512 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;
pub(crate) const DEEPSEEK_MAX_TOKENS: u16 = 512;
pub(crate) const DEEPSEEK_V4_FLASH_MODEL: &str = "deepseek-v4-flash";
const DEEPSEEK_CHAT_COMPLETIONS_URL: &str = "https://api.deepseek.com/chat/completions";

pub struct DeepSeekV4FlashConfig {
    pub(crate) api_key: SecretString,
}

impl DeepSeekV4FlashConfig {
    pub fn from_env() -> Result<Self, AgentError> {
        let api_key = env::var("DEEPSEEK_API_KEY")
            .map_err(|_| AgentError::new("DEEPSEEK_API_KEY is not set"))?;
        validate_api_key(&api_key)?;
        Ok(Self {
            api_key: SecretString::from(api_key),
        })
    }
}

pub(crate) enum CompletionProvider {
    Deterministic { response_delay: Duration },
    DeepSeekV4Flash(DeepSeekV4FlashProvider),
}

pub(crate) struct DeepSeekV4FlashProvider {
    client: Client,
    api_key: SecretString,
    endpoint: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct ModelMessage {
    pub(crate) role: ModelRole,
    pub(crate) content: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ModelRole {
    System,
    User,
    Assistant,
}

pub(crate) struct ModelContext {
    pub(crate) messages: Vec<ModelMessage>,
}

#[derive(Serialize)]
struct DeepSeekChatRequest<'a> {
    model: &'static str,
    messages: &'a [ModelMessage],
    thinking: DeepSeekThinking,
    stream: bool,
    max_tokens: u16,
}

#[derive(Serialize)]
struct DeepSeekThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct DeepSeekChatResponse {
    choices: Vec<DeepSeekChoice>,
}

#[derive(Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekResponseMessage,
    finish_reason: String,
}

#[derive(Deserialize)]
struct DeepSeekResponseMessage {
    content: Option<String>,
}

impl CompletionProvider {
    pub(crate) fn card_definition(&self) -> &'static str {
        match self {
            Self::Deterministic { .. } => BUILTIN_AGENT_DEFINITION,
            Self::DeepSeekV4Flash(_) => DEEPSEEK_V4_FLASH_AGENT_DEFINITION,
        }
    }

    pub(crate) async fn complete(&self, context: &ModelContext) -> Result<String, TurnFailure> {
        match self {
            Self::Deterministic { response_delay } => {
                sleep(*response_delay).await;
                Ok(deterministic_final(context))
            }
            Self::DeepSeekV4Flash(provider) => provider.complete(context).await,
        }
    }
}

impl DeepSeekV4FlashProvider {
    pub(crate) fn new(config: DeepSeekV4FlashConfig) -> Result<Self, AgentError> {
        Self::build(config, DEEPSEEK_CHAT_COMPLETIONS_URL.to_owned(), true)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        config: DeepSeekV4FlashConfig,
        endpoint: String,
    ) -> Result<Self, AgentError> {
        Self::build(config, endpoint, false)
    }

    fn build(
        config: DeepSeekV4FlashConfig,
        endpoint: String,
        https_only: bool,
    ) -> Result<Self, AgentError> {
        let client = Client::builder()
            .https_only(https_only)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|error| AgentError::context("could not construct DeepSeek client", error))?;
        Ok(Self {
            client,
            api_key: config.api_key,
            endpoint,
        })
    }

    async fn complete(&self, context: &ModelContext) -> Result<String, TurnFailure> {
        let body = encode_deepseek_request(context)?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
                .map_err(|_| TurnFailure::ProviderRejected)?;
        authorization.set_sensitive(true);

        let mut response = self
            .client
            .post(&self.endpoint)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| TurnFailure::ProviderUnavailable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_provider_status(status));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
        {
            return Err(TurnFailure::InvalidProviderResponse);
        }
        let mut payload = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TurnFailure::ProviderUnavailable)?
        {
            let remaining = MAX_PROVIDER_RESPONSE_BYTES
                .checked_sub(payload.len())
                .ok_or(TurnFailure::InvalidProviderResponse)?;
            if chunk.len() > remaining {
                return Err(TurnFailure::InvalidProviderResponse);
            }
            payload.extend_from_slice(&chunk);
        }
        decode_deepseek_response(&payload)
    }
}

fn validate_api_key(api_key: &str) -> Result<(), AgentError> {
    if api_key.trim().is_empty() {
        return Err(AgentError::new("DEEPSEEK_API_KEY must not be empty"));
    }
    if api_key.len() > 4 * 1024 || !api_key.is_ascii() || api_key.chars().any(char::is_control) {
        return Err(AgentError::new("DEEPSEEK_API_KEY is invalid"));
    }
    Ok(())
}

fn classify_provider_status(status: StatusCode) -> TurnFailure {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        TurnFailure::ProviderUnavailable
    } else {
        TurnFailure::ProviderRejected
    }
}

pub(crate) fn encode_deepseek_request(context: &ModelContext) -> Result<Vec<u8>, TurnFailure> {
    let request = DeepSeekChatRequest {
        model: DEEPSEEK_V4_FLASH_MODEL,
        messages: &context.messages,
        thinking: DeepSeekThinking { kind: "disabled" },
        stream: false,
        max_tokens: DEEPSEEK_MAX_TOKENS,
    };
    let body = serde_json::to_vec(&request).map_err(|_| TurnFailure::InvalidProviderResponse)?;
    if body.len() > MAX_PROVIDER_REQUEST_BYTES {
        return Err(TurnFailure::ContextLimit);
    }
    Ok(body)
}

fn decode_deepseek_response(payload: &[u8]) -> Result<String, TurnFailure> {
    let response: DeepSeekChatResponse =
        serde_json::from_slice(payload).map_err(|_| TurnFailure::InvalidProviderResponse)?;
    let [choice] = response.choices.as_slice() else {
        return Err(TurnFailure::InvalidProviderResponse);
    };
    if choice.finish_reason != "stop" {
        return Err(TurnFailure::InvalidProviderResponse);
    }
    let content = choice
        .message
        .content
        .as_deref()
        .ok_or(TurnFailure::InvalidProviderResponse)?;
    if !is_safe_model_content(content) {
        return Err(TurnFailure::InvalidProviderResponse);
    }
    Ok(content.to_owned())
}

fn is_safe_model_content(content: &str) -> bool {
    !content.trim().is_empty()
        && content.len() <= MAX_MODEL_CONTENT_BYTES
        && !content
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
}

fn deterministic_final(context: &ModelContext) -> String {
    let mut user_inputs = context
        .messages
        .iter()
        .rev()
        .filter(|message| matches!(message.role, ModelRole::User))
        .map(|message| message.content.as_str());
    let current = user_inputs
        .next()
        .expect("an admitted model context always contains the current user input");
    match user_inputs.next() {
        Some(previous) => format!("previous: {previous}; current: {current}"),
        None => format!("current: {current}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc as TestArc, Mutex as TestMutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use paraegox_deck::{Card, CardKey, DeckCompiler, DeckKey, DeckSpec};
    use paraegox_kernel::RuntimeHostId;
    use paraegox_runtime::{DeckLaunch, RuntimeHost, RuntimeHostIdentity};
    use secrecy::SecretString;
    use serde_json::Value;
    use uuid::Uuid;

    use super::*;
    use crate::{
        AgentCard, AgentService, CancelResult, SessionId, TurnId, TurnTerminal,
        deepseek_v4_flash_agent_definition, wire_safe_terminal,
    };

    const MAX_MODEL_CONTEXT_BYTES: usize = 256 * 1024;
    const BUILTIN_AGENT_SYSTEM_PROMPT: &str =
        "You are Paraegox, a concise embodied-intelligence agent. Answer the user directly.";

    struct CapturedRequest {
        body: Value,
        target: String,
        authorized: bool,
    }

    #[tokio::test]
    async fn deepseek_contract_is_bounded_safe_and_preserves_only_successful_history() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let api_key = Uuid::new_v4().to_string();
        let expected_authorization = format!("Bearer {api_key}");
        let requests = TestArc::new(TestMutex::new(Vec::new()));
        let captured_requests = TestArc::clone(&requests);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut index = 0usize;
            while index < 7 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "fake Provider did not receive all expected requests"
                        );
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fake Provider accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let request = read_json_request(&mut stream, &expected_authorization);
                captured_requests.lock().unwrap().push(request);
                let (delay, status, response) = match index {
                    0 => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"assistant one"},"finish_reason":"stop"}]}"#,
                    ),
                    1 => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"assistant two"},"finish_reason":"stop"}]}"#,
                    ),
                    2 => (
                        Duration::ZERO,
                        "503 Service Unavailable",
                        r#"{"error":"unavailable"}"#,
                    ),
                    3 => (
                        Duration::from_secs(1),
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"late timeout"},"finish_reason":"stop"}]}"#,
                    ),
                    4 => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"\u001b]0;unsafe\u0007"},"finish_reason":"stop"}]}"#,
                    ),
                    5 => (
                        Duration::from_secs(1),
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"late cancel"},"finish_reason":"stop"}]}"#,
                    ),
                    _ => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"assistant after failures"},"finish_reason":"stop"}]}"#,
                    ),
                };
                thread::sleep(delay);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                let _ = stream.flush();
                index += 1;
            }
        });

        let config = DeepSeekV4FlashConfig {
            api_key: SecretString::from(api_key),
        };
        let service = AgentService::with_deepseek_v4_flash_endpoint(config, endpoint).unwrap();
        let handle = service.handle();
        let key = CardKey::new("agent");
        let definition = deepseek_v4_flash_agent_definition();
        let compiler = DeckCompiler::new([definition.clone()]).unwrap();
        let lock = compiler
            .compile(&DeckSpec {
                key: DeckKey::new("deepseek-agent-test"),
                cards: vec![Card {
                    key: key.clone(),
                    definition,
                }],
            })
            .unwrap();
        let launch = DeckLaunch::new(
            lock,
            vec![Box::new(AgentCard::new_deepseek_v4_flash(
                key,
                handle.clone(),
            ))],
        )
        .unwrap();
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity("deepseek-agent-runtime"),
            vec![Box::new(service)],
            launch,
        )
        .unwrap();
        runtime.start().await.unwrap();

        let session = SessionId::new();
        assert_eq!(
            handle
                .submit_turn(session, TurnId::new(), "user one", Duration::from_secs(1))
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Final {
                content: "assistant one".to_owned()
            }
        );
        assert_eq!(
            handle
                .submit_turn(session, TurnId::new(), "user two", Duration::from_secs(1))
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Final {
                content: "assistant two".to_owned()
            }
        );
        let failed_turn = TurnId::new();
        let failed = handle
            .submit_turn(session, failed_turn, "user three", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            failed.terminal,
            TurnTerminal::Failed {
                reason: TurnFailure::ProviderUnavailable
            }
        );
        assert_eq!(
            handle
                .submit_turn(session, failed_turn, "user three", Duration::from_secs(1),)
                .await
                .unwrap(),
            failed,
            "a failed terminal must replay without another provider request"
        );

        assert_eq!(
            requests.lock().unwrap().len(),
            3,
            "replaying a failed Turn must not send another HTTP request"
        );

        let timed_out = handle
            .submit_turn(
                session,
                TurnId::new(),
                "must time out",
                Duration::from_millis(500),
            )
            .await
            .unwrap();
        assert_eq!(timed_out.terminal, TurnTerminal::TimedOut);

        let unsafe_output = handle
            .submit_turn(
                session,
                TurnId::new(),
                "reject unsafe output",
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(
            unsafe_output.terminal,
            TurnTerminal::Failed {
                reason: TurnFailure::InvalidProviderResponse
            }
        );

        let cancelled_turn = TurnId::new();
        let submitting_handle = handle.clone();
        let cancelled_submit = tokio::spawn(async move {
            submitting_handle
                .submit_turn(
                    session,
                    cancelled_turn,
                    "must cancel",
                    Duration::from_secs(3),
                )
                .await
        });
        wait_for_request_count(&requests, 6).await;
        assert_eq!(
            handle.cancel(session, cancelled_turn).unwrap(),
            CancelResult::CancellationRequested
        );
        assert_eq!(
            cancelled_submit.await.unwrap().unwrap().terminal,
            TurnTerminal::Cancelled
        );

        assert_eq!(
            handle
                .submit_turn(
                    session,
                    TurnId::new(),
                    "after failures",
                    Duration::from_secs(3),
                )
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Final {
                content: "assistant after failures".to_owned()
            }
        );

        let escape_heavy = ModelContext {
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: "\\".repeat(MAX_MODEL_CONTEXT_BYTES),
            }],
        };
        assert_eq!(
            encode_deepseek_request(&escape_heavy).unwrap_err(),
            TurnFailure::ContextLimit,
            "the encoded HTTP request has its own bound"
        );
        assert!(matches!(
            wire_safe_terminal(
                SessionId::new(),
                TurnId::new(),
                TurnTerminal::Final {
                    content: "\"".repeat(MAX_MODEL_CONTENT_BYTES),
                },
            ),
            TurnTerminal::Final { .. }
        ));

        runtime.stop().await.unwrap();
        server.join().unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 7, "each new Turn makes exactly one request");
        assert!(requests.iter().all(|request| request.authorized));
        assert!(
            requests
                .iter()
                .all(|request| request.target == "/chat/completions")
        );
        let request = &requests[2].body;
        assert_eq!(request["model"], DEEPSEEK_V4_FLASH_MODEL);
        assert_eq!(request["thinking"]["type"], "disabled");
        assert_eq!(request["stream"], false);
        assert_eq!(request["max_tokens"], DEEPSEEK_MAX_TOKENS);
        assert!(request.get("tools").is_none());
        assert_eq!(
            request["messages"],
            serde_json::json!([
                {"role": "system", "content": BUILTIN_AGENT_SYSTEM_PROMPT},
                {"role": "user", "content": "user one"},
                {"role": "assistant", "content": "assistant one"},
                {"role": "user", "content": "user two"},
                {"role": "assistant", "content": "assistant two"},
                {"role": "user", "content": "user three"}
            ])
        );
        assert_eq!(
            requests[6].body["messages"],
            serde_json::json!([
                {"role": "system", "content": BUILTIN_AGENT_SYSTEM_PROMPT},
                {"role": "user", "content": "user one"},
                {"role": "assistant", "content": "assistant one"},
                {"role": "user", "content": "user two"},
                {"role": "assistant", "content": "assistant two"},
                {"role": "user", "content": "after failures"}
            ]),
            "failed, timed-out, unsafe, and cancelled Turns must not enter history"
        );
    }

    fn runtime_identity(label: &str) -> RuntimeHostIdentity {
        RuntimeHostIdentity::new(RuntimeHostId::new(label).unwrap())
    }

    async fn wait_for_request_count(
        requests: &TestArc<TestMutex<Vec<CapturedRequest>>>,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if requests.lock().unwrap().len() >= expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake Provider request count did not advance");
    }

    fn read_json_request(stream: &mut TcpStream, expected_authorization: &str) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut received = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP request ended before its headers");
            received.extend_from_slice(&chunk[..read]);
            if let Some(position) = received.windows(4).position(|part| part == b"\r\n\r\n") {
                break position + 4;
            }
            assert!(
                received.len() <= 16 * 1024,
                "HTTP request headers exceeded the test bound"
            );
        };
        let headers = String::from_utf8(received[..header_end].to_vec()).unwrap();
        let mut lines = headers.lines();
        let target = lines
            .next()
            .and_then(|request_line| request_line.split_whitespace().nth(1))
            .expect("request should carry a target")
            .to_owned();
        let headers: Vec<_> = lines.collect();
        let content_length = headers
            .iter()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .expect("request should carry Content-Length");
        assert!(
            content_length <= MAX_PROVIDER_REQUEST_BYTES,
            "HTTP request body exceeded the test bound"
        );
        while received.len() < header_end + content_length {
            let mut chunk = [0u8; 4096];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP request ended before its body");
            received.extend_from_slice(&chunk[..read]);
        }
        let authorized = headers.iter().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("authorization") && value.trim() == expected_authorization
            })
        });
        CapturedRequest {
            body: serde_json::from_slice(&received[header_end..header_end + content_length])
                .unwrap(),
            target,
            authorized,
        }
    }
}
