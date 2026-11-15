//! `POST /jev/systemone`: a relay from the browser to TypeSafe's Jev
//! SystemOne API, which refuses browser CORS.
//!
//! The official lobby hostnames are served by the Cloudflare Worker in
//! `lobby-worker`, not by this server, so the Worker carries a behavioral twin of
//! this route (`lobby-worker/src/jev-relay.ts`). A change to the envelope, the
//! invariants below, or the answers must be made in both.
//!
//! The browser sends a CORS *simple request* — `POST` with
//! `Content-Type: text/plain;charset=UTF-8` and no `Authorization` or other
//! custom header — so no preflight is issued and the server's existing CORS
//! layer is enough to make the answer readable cross-origin. The body is the
//! envelope `{"apiKey": "<key>", "request": { …SystemOne body… }}`. The relay
//! forwards `request` upstream with `Authorization: Bearer <apiKey>` and hands
//! the upstream's status, `Content-Type` and body back.
//!
//! The raw body is read as bytes and parsed as JSON here rather than through
//! axum's `Json` extractor, because `Json` rejects the `text/plain` label a
//! simple request has to carry.
//!
//! Security invariants:
//! - The key comes only from the envelope. No request header is read, so an
//!   incoming `Authorization` header is structurally ignored.
//! - The key is never logged, stored, echoed in a response, or forwarded as a
//!   body field: it exists for one request, only on the handler's stack and in
//!   the sensitive-marked upstream `Authorization` header. Envelope parse
//!   errors are discarded unformatted, since serde errors quote input values.
//! - The upstream URL comes only from `TYPESAFE_API_URL` (resolved once at
//!   startup), never from the request.
//! - Redirects are not followed, and no upstream header other than
//!   `Content-Type` is copied into the response — in particular never
//!   `Location`, which would make the browser re-send the key-bearing envelope
//!   to an upstream-chosen URL.
//!
//! `TYPESAFE_API_URL` (and [`JevRelayConfig::with_upstream_override`]) accept
//! `http://` so tests and local setups can point at a loopback stub; an
//! `http://` upstream sends the bearer key in cleartext and is for tests and
//! local use only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, FromRef, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{post, MethodRouter};
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderValue, StatusCode};
use serde::Deserialize;
use tokio::sync::Semaphore;
use tracing::info;
use url::Url;

use crate::metrics::JevRelayStatus;
use crate::{AppState, ServerContext};

/// Largest envelope the relay accepts; larger bodies are answered 413.
pub const MAX_BODY_BYTES: usize = 512 * 1024;
/// Upstream used when `TYPESAFE_API_URL` is unset or blank.
pub const DEFAULT_UPSTREAM: &str = "https://api.typesafe.ai/v1/systemone";
/// The environment variable that overrides the upstream URL.
pub const UPSTREAM_ENV: &str = "TYPESAFE_API_URL";
/// Total time allowed for one upstream call, body included.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// Upstream calls allowed in flight at once across the process.
pub const DEFAULT_MAX_IN_FLIGHT: usize = 64;

const BAD_REQUEST_BODY: &str = "invalid Jev relay request";
const TOO_LARGE_BODY: &str = "Jev relay request body too large";
const BUSY_BODY: &str = "Jev relay busy";
const TIMEOUT_BODY: &str = "Jev upstream timed out";
const UNREACHABLE_BODY: &str = "Jev upstream unreachable";

/// Where and how the relay calls upstream. Resolved once at startup.
#[derive(Debug, Clone)]
pub struct JevRelayConfig {
    pub upstream: Url,
    pub timeout: Duration,
    pub max_in_flight: usize,
}

impl Default for JevRelayConfig {
    fn default() -> Self {
        Self {
            upstream: Url::parse(DEFAULT_UPSTREAM).expect("the default Jev upstream URL parses"),
            timeout: DEFAULT_TIMEOUT,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
        }
    }
}

impl JevRelayConfig {
    /// Resolve the upstream from an optional override value: unset or blank
    /// keeps the default; anything else must be an `http(s)` URL.
    pub fn with_upstream_override(value: Option<&str>) -> Result<Self, String> {
        let value = value.map(str::trim).unwrap_or_default();
        if value.is_empty() {
            return Ok(Self::default());
        }
        let upstream = Url::parse(value)
            .map_err(|error| format!("{UPSTREAM_ENV} is not a valid URL: {error}"))?;
        // A URL carrying userinfo would make reqwest add its own Basic
        // `Authorization` next to the user's Bearer key upstream.
        if !upstream.username().is_empty() || upstream.password().is_some() {
            return Err(format!(
                "{UPSTREAM_ENV} must not carry a username or password"
            ));
        }
        match upstream.scheme() {
            "http" | "https" => Ok(Self {
                upstream,
                ..Self::default()
            }),
            scheme => Err(format!(
                "{UPSTREAM_ENV} must be an http or https URL, not {scheme}"
            )),
        }
    }

    /// Resolve the configuration from `TYPESAFE_API_URL`.
    pub fn from_env() -> Result<Self, String> {
        Self::with_upstream_override(std::env::var(UPSTREAM_ENV).ok().as_deref())
    }
}

/// The relay's per-process handle: its HTTP client, upstream and in-flight
/// cap. Cloning is a few reference-count bumps.
#[derive(Clone)]
pub struct JevRelay {
    client: reqwest::Client,
    upstream: Arc<Url>,
    permits: Arc<Semaphore>,
}

impl JevRelay {
    pub fn new(config: JevRelayConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            // A followed redirect would carry the bearer key to a URL the
            // upstream chose; the 3xx is returned (without `Location`) instead.
            .redirect(reqwest::redirect::Policy::none())
            // The key's path is decided by `TYPESAFE_API_URL` alone, never by
            // proxy environment variables.
            .no_proxy()
            .build()?;
        Ok(Self {
            client,
            upstream: Arc::new(config.upstream),
            permits: Arc::new(Semaphore::new(config.max_in_flight)),
        })
    }

    /// The upstream's origin (scheme, host, port) for the startup log line.
    /// Excludes any userinfo and the path.
    pub fn upstream_origin(&self) -> String {
        self.upstream.origin().ascii_serialization()
    }

    /// Validate the envelope, forward it, and build the answer. Every exit
    /// returns its outcome so the caller counts it.
    async fn forward(&self, body: Result<Bytes, BytesRejection>) -> (JevRelayStatus, Response) {
        let bytes = match body {
            Ok(bytes) => bytes,
            Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                return refuse(JevRelayStatus::PayloadTooLarge);
            }
            Err(_) => return refuse(JevRelayStatus::BadRequest),
        };
        // The error is discarded unformatted: serde errors quote input values,
        // which may be the key.
        let Ok(envelope) = serde_json::from_slice::<JevRelayEnvelope>(&bytes) else {
            return refuse(JevRelayStatus::BadRequest);
        };
        if envelope.api_key.trim().is_empty() {
            return refuse(JevRelayStatus::BadRequest);
        }
        // The key is forwarded verbatim; a value that is not a legal header
        // (e.g. it contains a control character) is refused.
        let Ok(mut bearer) = HeaderValue::try_from(format!("Bearer {}", envelope.api_key)) else {
            return refuse(JevRelayStatus::BadRequest);
        };
        bearer.set_sensitive(true);
        let Ok(upstream_body) = serde_json::to_vec(&envelope.request) else {
            return refuse(JevRelayStatus::BadRequest);
        };
        // Held until the upstream answer has been read in full.
        let Ok(_permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            return refuse(JevRelayStatus::Busy);
        };

        let sent = self
            .client
            .post(self.upstream.as_str())
            .header(AUTHORIZATION, bearer)
            .header(CONTENT_TYPE, "application/json")
            .body(upstream_body)
            .send()
            .await;
        let upstream = match sent {
            Ok(upstream) => upstream,
            Err(error) => return refuse(transport_failure(&error)),
        };
        let status = upstream.status();
        // Only `Content-Type` is copied. No other upstream header is — in
        // particular never `Location`, since a 3xx carrying it would make the
        // browser re-send the key-bearing envelope to the upstream-chosen URL.
        let content_type = upstream.headers().get(CONTENT_TYPE).cloned();
        let bytes = match upstream.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return refuse(transport_failure(&error)),
        };
        let mut response = Response::new(Body::from(bytes));
        *response.status_mut() = status;
        if let Some(content_type) = content_type {
            response.headers_mut().insert(CONTENT_TYPE, content_type);
        }
        (JevRelayStatus::from_upstream(status), response)
    }
}

impl Default for JevRelay {
    fn default() -> Self {
        Self::new(JevRelayConfig::default()).expect("the default Jev relay HTTP client builds")
    }
}

/// The request body. Derives only `Deserialize` — no `Debug`, `Clone` or
/// `Serialize` — so the key-bearing value cannot be formatted into a log line
/// or copied by accident. `request` is typed as an object map so a non-object
/// request is a parse error rather than a runtime check.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JevRelayEnvelope {
    #[serde(rename = "apiKey")]
    api_key: String,
    request: serde_json::Map<String, serde_json::Value>,
}

impl FromRef<AppState> for ServerContext {
    fn from_ref(app: &AppState) -> Self {
        app.context.clone()
    }
}

/// The relay's method router, body limit included, so production and tests
/// mount the identical route. Generic over any router state that yields a
/// [`ServerContext`].
pub fn method_router<S>() -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
    ServerContext: FromRef<S>,
{
    post(relay).layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// Takes no `HeaderMap`: nothing from the request's headers can reach the
/// upstream call. Logs status and latency only — the module's one log line.
async fn relay(
    State(context): State<ServerContext>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let started = Instant::now();
    let (status, response) = context.jev_relay.forward(body).await;
    context.metrics.record_jev_relay(status);
    info!(
        status = response.status().as_u16(),
        outcome = status.label(),
        latency_ms = started.elapsed().as_millis() as u64,
        "jev relay request"
    );
    response
}

/// A transport failure: a timeout is a 504, anything else a 502.
fn transport_failure(error: &reqwest::Error) -> JevRelayStatus {
    if error.is_timeout() {
        JevRelayStatus::UpstreamTimeout
    } else {
        JevRelayStatus::UpstreamUnreachable
    }
}

/// The relay's own answer for an outcome it produces itself. Fixed text only:
/// no part of the request is ever echoed.
fn refuse(status: JevRelayStatus) -> (JevRelayStatus, Response) {
    let (code, body) = match status {
        JevRelayStatus::BadRequest => (StatusCode::BAD_REQUEST, BAD_REQUEST_BODY),
        JevRelayStatus::PayloadTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, TOO_LARGE_BODY),
        JevRelayStatus::Busy => (StatusCode::SERVICE_UNAVAILABLE, BUSY_BODY),
        JevRelayStatus::UpstreamTimeout => (StatusCode::GATEWAY_TIMEOUT, TIMEOUT_BODY),
        JevRelayStatus::UpstreamUnreachable => (StatusCode::BAD_GATEWAY, UNREACHABLE_BODY),
        // Upstream outcomes are passed through, never produced by the relay.
        JevRelayStatus::Upstream2xx
        | JevRelayStatus::Upstream4xx
        | JevRelayStatus::Upstream5xx
        | JevRelayStatus::UpstreamOther => (StatusCode::BAD_GATEWAY, UNREACHABLE_BODY),
    };
    (status, (code, body).into_response())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{Body, Bytes};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::Response;
    use tokio::task::JoinHandle;
    use url::Url;

    use super::JevRelayConfig;

    /// A key no test would ever produce by accident, searched for in every
    /// response body, upstream body and captured log.
    pub(crate) const TEST_KEY: &str = "sk-test-relay-SECRET-7f3a";

    /// How the mock upstream answers every request.
    #[derive(Clone, Copy)]
    pub(crate) enum MockReply {
        Respond {
            status: StatusCode,
            content_type: Option<&'static str>,
            body: &'static str,
        },
        Redirect {
            location: &'static str,
        },
        Hang,
    }

    /// Every request the mock upstream received: headers and raw body.
    pub(crate) type UpstreamLog = Arc<std::sync::Mutex<Vec<(HeaderMap, Vec<u8>)>>>;

    /// A mock upstream on an ephemeral loopback port. It records every
    /// request on any path (so a followed redirect would be counted) before
    /// answering. Returns the relay-facing `/v1/systemone` URL.
    pub(crate) async fn spawn_recording_upstream(
        reply: MockReply,
    ) -> (Url, UpstreamLog, JoinHandle<()>) {
        let log: UpstreamLog = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_log = log.clone();
        let app = axum::Router::new().fallback(move |headers: HeaderMap, body: Bytes| {
            let log = handler_log.clone();
            async move {
                log.lock()
                    .expect("upstream log")
                    .push((headers, body.to_vec()));
                match reply {
                    MockReply::Respond {
                        status,
                        content_type,
                        body,
                    } => {
                        let mut builder = Response::builder().status(status);
                        if let Some(content_type) = content_type {
                            builder = builder.header(http::header::CONTENT_TYPE, content_type);
                        }
                        builder.body(Body::from(body)).expect("mock response")
                    }
                    MockReply::Redirect { location } => Response::builder()
                        .status(StatusCode::TEMPORARY_REDIRECT)
                        .header(http::header::LOCATION, location)
                        .body(Body::empty())
                        .expect("mock redirect"),
                    MockReply::Hang => std::future::pending().await,
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock upstream");
        });
        let url = Url::parse(&format!("http://{addr}/v1/systemone")).expect("mock upstream url");
        (url, log, server)
    }

    /// Polls to a deadline rather than sleeping a fixed interval.
    pub(crate) async fn wait_for_hits(log: &UpstreamLog, want: usize) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let count = hits(log);
            if count >= want {
                return count;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {count} of {want} upstream requests arrived within 2s"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub(crate) fn hits(log: &UpstreamLog) -> usize {
        log.lock().expect("upstream log").len()
    }

    /// A relay configuration pointed at `upstream`, with a short timeout.
    pub(crate) fn relay_config(upstream: Url) -> JevRelayConfig {
        JevRelayConfig {
            upstream,
            timeout: Duration::from_millis(300),
            max_in_flight: 64,
        }
    }

    /// The simple-request envelope carrying [`TEST_KEY`].
    pub(crate) fn envelope(request: serde_json::Value) -> String {
        serde_json::json!({ "apiKey": TEST_KEY, "request": request }).to_string()
    }

    /// A test HTTP client: follows no redirects and ignores proxy settings.
    pub(crate) fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("test client")
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use axum::Router;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::task::JoinHandle;
    use url::Url;

    use super::test_support::{
        client, envelope, hits, relay_config, spawn_recording_upstream, wait_for_hits, MockReply,
        TEST_KEY,
    };
    use super::*;

    const SIMPLE_CONTENT_TYPE: &str = "text/plain;charset=UTF-8";

    fn context_for(config: JevRelayConfig) -> ServerContext {
        ServerContext {
            jev_relay: JevRelay::new(config).expect("relay client"),
            ..ServerContext::default()
        }
    }

    /// Serves the production method router under a bare `ServerContext`.
    async fn spawn_relay(context: ServerContext) -> (String, JoinHandle<()>) {
        let app = Router::new()
            .route("/jev/systemone", method_router())
            .with_state(context);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind relay");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("relay server");
        });
        (format!("http://{addr}/jev/systemone"), server)
    }

    /// Posts `body` as a CORS simple request, optionally with extra headers.
    async fn post_simple(
        url: &str,
        body: impl Into<reqwest::Body>,
        extra: &[(&str, &str)],
    ) -> (StatusCode, reqwest::header::HeaderMap, String) {
        let mut request = client()
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, SIMPLE_CONTENT_TYPE)
            .body(body);
        for (name, value) in extra {
            request = request.header(*name, *value);
        }
        let response = request.send().await.expect("relay request");
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.expect("relay body");
        (status, headers, text)
    }

    fn ok_json() -> MockReply {
        MockReply::Respond {
            status: StatusCode::OK,
            content_type: Some("application/json"),
            body: r#"{"pick":1}"#,
        }
    }

    #[tokio::test]
    async fn malformed_envelopes_are_refused_before_upstream() {
        let (upstream, log, mock) = spawn_recording_upstream(ok_json()).await;
        let context = context_for(relay_config(upstream));
        let (url, relay) = spawn_relay(context.clone()).await;

        let no_key = serde_json::json!({ "request": { "model": "jev" } }).to_string();
        let bearer = format!("Bearer {TEST_KEY}");
        /// (label, request body, extra request headers)
        type Case<'a> = (&'a str, String, Vec<(&'a str, &'a str)>);
        let cases: Vec<Case<'_>> = vec![
            ("missing apiKey", no_key.clone(), vec![]),
            ("empty body", String::new(), vec![]),
            (
                "whitespace-only key",
                serde_json::json!({ "apiKey": "   ", "request": {} }).to_string(),
                vec![],
            ),
            ("not JSON", format!("apiKey={TEST_KEY}"), vec![]),
            (
                "unknown field",
                serde_json::json!({
                    "apiKey": TEST_KEY,
                    "request": {},
                    "upstream": "http://evil.example/steal",
                })
                .to_string(),
                vec![],
            ),
            (
                // serde's own error text would quote this value.
                "non-object request",
                serde_json::json!({ "apiKey": TEST_KEY, "request": TEST_KEY }).to_string(),
                vec![],
            ),
            (
                "key with a newline",
                serde_json::json!({ "apiKey": format!("{TEST_KEY}\n"), "request": {} }).to_string(),
                vec![],
            ),
            (
                "Authorization header without an envelope key",
                no_key,
                vec![("Authorization", bearer.as_str())],
            ),
        ];
        let refused = cases.len() as u64;
        for (name, body, extra) in cases {
            let (status, _, text) = post_simple(&url, body, &extra).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
            assert_eq!(text, BAD_REQUEST_BODY, "{name}");
            assert!(!text.contains(TEST_KEY), "{name} echoed the key");
            assert_eq!(hits(&log), 0, "{name} reached the upstream");
        }
        assert_eq!(
            context.metrics.jev_relay_count(JevRelayStatus::BadRequest),
            refused
        );

        // Reach guard: a valid envelope through the same relay does reach it.
        let (status, _, _) =
            post_simple(&url, envelope(serde_json::json!({ "model": "jev" })), &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hits(&log), 1);
        assert_eq!(
            context.metrics.jev_relay_count(JevRelayStatus::Upstream2xx),
            1
        );

        relay.abort();
        mock.abort();
    }

    #[tokio::test]
    async fn valid_envelope_forwards_bearer_and_inner_request_only() {
        let (upstream, log, mock) = spawn_recording_upstream(ok_json()).await;
        let context = context_for(relay_config(upstream));
        let (url, relay) = spawn_relay(context.clone()).await;

        let inner = serde_json::json!({
            "model": "jev",
            "question": { "pack": ["a", "b"], "pool": [] },
        });
        let (status, headers, text) = post_simple(
            &url,
            envelope(inner.clone()),
            &[("Authorization", "Bearer DECOY")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(text, r#"{"pick":1}"#);
        assert_eq!(
            headers
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        assert!(!text.contains(TEST_KEY));

        let records = log.lock().expect("upstream log").clone();
        assert_eq!(records.len(), 1, "exactly one upstream call");
        let (upstream_headers, upstream_body) = &records[0];
        assert_eq!(
            upstream_headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some(format!("Bearer {TEST_KEY}").as_str()),
            "the envelope key is the bearer, not the incoming header"
        );
        assert_eq!(
            upstream_headers
                .get_all(http::header::AUTHORIZATION)
                .iter()
                .count(),
            1
        );
        assert_eq!(
            upstream_headers
                .get(http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        for (name, value) in upstream_headers {
            assert!(
                !value.as_bytes().windows(5).any(|w| w == b"DECOY"),
                "decoy reached the upstream in {name}"
            );
        }
        let forwarded: serde_json::Value =
            serde_json::from_slice(upstream_body).expect("upstream body is JSON");
        assert_eq!(
            forwarded, inner,
            "upstream body is exactly the inner request"
        );
        let upstream_text = String::from_utf8_lossy(upstream_body);
        assert!(
            !upstream_text.contains(TEST_KEY),
            "key leaked into the body"
        );
        assert!(!upstream_text.contains("apiKey"));
        assert_eq!(
            context.metrics.jev_relay_count(JevRelayStatus::Upstream2xx),
            1
        );

        relay.abort();
        mock.abort();
    }

    #[tokio::test]
    async fn upstream_status_content_type_and_body_pass_through() {
        let cases = [
            (
                MockReply::Respond {
                    status: StatusCode::UNPROCESSABLE_ENTITY,
                    content_type: Some("application/problem+json"),
                    body: r#"{"title":"bad pack"}"#,
                },
                JevRelayStatus::Upstream4xx,
            ),
            (
                MockReply::Respond {
                    status: StatusCode::OK,
                    content_type: None,
                    body: "no content type",
                },
                JevRelayStatus::Upstream2xx,
            ),
            (
                MockReply::Respond {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    content_type: Some("text/plain"),
                    body: "upstream down",
                },
                JevRelayStatus::Upstream5xx,
            ),
        ];
        for (reply, outcome) in cases {
            let MockReply::Respond {
                status: want_status,
                content_type: want_type,
                body: want_body,
            } = reply
            else {
                unreachable!("every case responds");
            };
            let (upstream, log, mock) = spawn_recording_upstream(reply).await;
            let context = context_for(relay_config(upstream));
            let (url, relay) = spawn_relay(context.clone()).await;

            let (status, headers, text) =
                post_simple(&url, envelope(serde_json::json!({ "model": "jev" })), &[]).await;
            assert_eq!(status, want_status);
            assert_eq!(text, want_body);
            assert_eq!(
                headers
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok()),
                want_type,
                "Content-Type for {want_status}"
            );
            assert!(!text.contains(TEST_KEY));
            assert_eq!(hits(&log), 1);
            assert_eq!(context.metrics.jev_relay_count(outcome), 1);

            relay.abort();
            mock.abort();
        }
    }

    /// Log sink shared between the test and the subscriber it installs.
    #[derive(Clone, Default)]
    struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Current-thread runtime (the default): the relay, the mocks and the
    /// clients all run on this thread, so the thread-scoped subscriber sees
    /// every event they emit, at every level and from every target.
    #[tokio::test]
    async fn relay_logs_status_and_latency_but_never_the_key() {
        let capture = Capture::default();
        let sink = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let decoy = [("Authorization", "Bearer DECOY")];

        let (upstream, _log, mock) = spawn_recording_upstream(ok_json()).await;
        let (url, relay) = spawn_relay(context_for(relay_config(upstream))).await;

        // The relay's log callsite is shared with the tests running on other
        // threads. If one of them registers it at the moment this test's
        // subscriber is installed, tracing can cache the other thread's
        // "not interested" verdict for it. One warm-up request makes sure the
        // callsite is registered; rebuilding the interest cache afterwards
        // then includes this subscriber, and no later registration can
        // overwrite it.
        let (status, _, _) = post_simple(
            &url,
            envelope(serde_json::json!({ "model": "jev" })),
            &decoy,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        tracing::callsite::rebuild_interest_cache();

        // Success, the serde-echo 400 and the 413 through a live upstream.
        let (status, _, _) = post_simple(
            &url,
            envelope(serde_json::json!({ "model": "jev" })),
            &decoy,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let echo = serde_json::json!({ "apiKey": TEST_KEY, "request": TEST_KEY }).to_string();
        let (status, _, _) = post_simple(&url, echo, &decoy).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let oversize = envelope(serde_json::json!({ "pad": "k".repeat(600 * 1024) }));
        let (status, _, _) = post_simple(&url, oversize, &decoy).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

        // 504 and 503 through a hung upstream at a cap of one.
        let (hung, hung_log, hung_mock) = spawn_recording_upstream(MockReply::Hang).await;
        let config = JevRelayConfig {
            max_in_flight: 1,
            ..relay_config(hung)
        };
        let (hung_url, hung_relay) = spawn_relay(context_for(config)).await;
        let first_url = hung_url.clone();
        let first = tokio::spawn(async move {
            post_simple(
                &first_url,
                envelope(serde_json::json!({ "model": "jev" })),
                &decoy,
            )
            .await
            .0
        });
        wait_for_hits(&hung_log, 1).await;
        let (status, _, _) = post_simple(
            &hung_url,
            envelope(serde_json::json!({ "model": "jev" })),
            &decoy,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            first.await.expect("first request"),
            StatusCode::GATEWAY_TIMEOUT
        );

        let captured = String::from_utf8_lossy(&capture.0.lock().expect("capture")).into_owned();
        // Reach guard: the relay's own line fired on every path.
        for code in [200, 400, 413, 504, 503] {
            assert!(
                captured
                    .lines()
                    .any(|line| line.contains("jev relay request")
                        && line.contains(&format!("status={code}"))),
                "no relay line with status={code} in:\n{captured}"
            );
        }
        assert!(
            !captured.contains(TEST_KEY),
            "key found in logs:\n{captured}"
        );
        assert!(
            !captured.contains("DECOY"),
            "decoy found in logs:\n{captured}"
        );

        relay.abort();
        mock.abort();
        hung_relay.abort();
        hung_mock.abort();
    }

    #[tokio::test]
    async fn body_limit_is_512_kib() {
        let (upstream, log, mock) = spawn_recording_upstream(ok_json()).await;
        let context = context_for(relay_config(upstream));
        let (url, relay) = spawn_relay(context.clone()).await;

        // Just under the limit passes.
        let fits = envelope(serde_json::json!({ "pad": "a".repeat(500 * 1024) }));
        assert!(fits.len() < MAX_BODY_BYTES);
        let (status, _, _) = post_simple(&url, fits, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hits(&log), 1);

        // Over the limit, with a Content-Length.
        let oversize = envelope(serde_json::json!({ "pad": "a".repeat(600 * 1024) }));
        let (status, _, text) = post_simple(&url, oversize.clone(), &[]).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(text, TOO_LARGE_BODY);
        assert!(!text.contains(TEST_KEY));
        assert_eq!(
            hits(&log),
            1,
            "an oversize body must not reach the upstream"
        );

        // Over the limit without a Content-Length: the limit counts bytes read.
        let (status_line, body) = post_chunked(&url, oversize.as_bytes()).await;
        assert!(
            status_line.starts_with("HTTP/1.1 413"),
            "chunked oversize answered {status_line}"
        );
        assert!(!body.contains(TEST_KEY));
        assert_eq!(hits(&log), 1);
        assert_eq!(
            context
                .metrics
                .jev_relay_count(JevRelayStatus::PayloadTooLarge),
            2
        );

        relay.abort();
        mock.abort();
    }

    /// Sends `body` with `Transfer-Encoding: chunked` over a raw socket and
    /// returns the status line and the rest of the response. Writes on a
    /// separate task so an early answer is read even if the relay stops
    /// reading the body.
    async fn post_chunked(url: &str, body: &[u8]) -> (String, String) {
        let url = Url::parse(url).expect("url");
        let host = url.host_str().expect("host").to_string();
        let port = url.port().expect("port");
        let stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .expect("connect");
        let (mut reader, mut writer) = stream.into_split();
        let mut request = Vec::new();
        write!(
            request,
            "POST {} HTTP/1.1\r\nHost: {host}\r\nContent-Type: {SIMPLE_CONTENT_TYPE}\r\n\
             Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            url.path()
        )
        .expect("head");
        for chunk in body.chunks(16 * 1024) {
            write!(request, "{:x}\r\n", chunk.len()).expect("chunk size");
            request.extend_from_slice(chunk);
            request.extend_from_slice(b"\r\n");
        }
        request.extend_from_slice(b"0\r\n\r\n");
        let writer_task = tokio::spawn(async move {
            // The relay may answer and stop reading before the body ends.
            let _ = writer.write_all(&request).await;
        });
        let mut response = Vec::new();
        let _ = reader.read_to_end(&mut response).await;
        writer_task.abort();
        let text = String::from_utf8_lossy(&response).into_owned();
        let status_line = text.lines().next().unwrap_or_default().to_string();
        (status_line, text)
    }

    #[tokio::test]
    async fn hung_upstream_times_out_as_504() {
        let (upstream, log, mock) = spawn_recording_upstream(MockReply::Hang).await;
        let context = context_for(relay_config(upstream));
        let (url, relay) = spawn_relay(context.clone()).await;

        let started = std::time::Instant::now();
        let (status, _, text) =
            post_simple(&url, envelope(serde_json::json!({ "model": "jev" })), &[]).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(text, TIMEOUT_BODY);
        assert!(!text.contains(TEST_KEY));
        assert!(started.elapsed() < Duration::from_secs(5));
        // Reach guard: the request did reach the (hung) upstream.
        assert_eq!(hits(&log), 1);
        assert_eq!(
            context
                .metrics
                .jev_relay_count(JevRelayStatus::UpstreamTimeout),
            1
        );
        relay.abort();
        mock.abort();

        // Sibling: nothing listening → 502, not 504.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = closed.local_addr().expect("local addr");
        drop(closed);
        let dead = Url::parse(&format!("http://{addr}/v1/systemone")).expect("url");
        let context = context_for(relay_config(dead));
        let (url, relay) = spawn_relay(context.clone()).await;
        let (status, _, text) =
            post_simple(&url, envelope(serde_json::json!({ "model": "jev" })), &[]).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(text, UNREACHABLE_BODY);
        assert_eq!(
            context
                .metrics
                .jev_relay_count(JevRelayStatus::UpstreamUnreachable),
            1
        );
        relay.abort();
    }

    #[tokio::test]
    async fn requests_over_the_in_flight_cap_are_refused_busy() {
        let (upstream, log, mock) = spawn_recording_upstream(MockReply::Hang).await;
        let context = context_for(JevRelayConfig {
            max_in_flight: 1,
            ..relay_config(upstream)
        });
        let (url, relay) = spawn_relay(context.clone()).await;
        let body = || envelope(serde_json::json!({ "model": "jev" }));

        let first_url = url.clone();
        let first_body = body();
        let first = tokio::spawn(async move { post_simple(&first_url, first_body, &[]).await.0 });
        wait_for_hits(&log, 1).await;

        let (status, _, text) = post_simple(&url, body(), &[]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(text, BUSY_BODY);
        assert_eq!(
            hits(&log),
            1,
            "a refused request must not reach the upstream"
        );
        assert_eq!(context.metrics.jev_relay_count(JevRelayStatus::Busy), 1);

        assert_eq!(
            first.await.expect("first request"),
            StatusCode::GATEWAY_TIMEOUT
        );
        // The permit is released: a third request reaches the upstream again.
        let (status, _, _) = post_simple(&url, body(), &[]).await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(hits(&log), 2);
        assert_eq!(
            context
                .metrics
                .jev_relay_count(JevRelayStatus::UpstreamTimeout),
            2
        );

        relay.abort();
        mock.abort();
    }

    #[tokio::test]
    async fn upstream_redirects_are_passed_through_not_followed() {
        let (upstream, log, mock) = spawn_recording_upstream(MockReply::Redirect {
            location: "/elsewhere",
        })
        .await;
        let context = context_for(relay_config(upstream));
        let (url, relay) = spawn_relay(context.clone()).await;

        let (status, headers, text) =
            post_simple(&url, envelope(serde_json::json!({ "model": "jev" })), &[]).await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert!(
            headers.get(reqwest::header::LOCATION).is_none(),
            "the upstream Location must not reach the browser"
        );
        assert!(!text.contains(TEST_KEY));
        // The mock records every path, so a followed redirect would count twice.
        assert_eq!(hits(&log), 1);
        assert_eq!(
            context
                .metrics
                .jev_relay_count(JevRelayStatus::UpstreamOther),
            1
        );

        relay.abort();
        mock.abort();
    }

    #[test]
    fn upstream_override_resolution() {
        for unset in [None, Some(""), Some("  ")] {
            let config = JevRelayConfig::with_upstream_override(unset).expect("default");
            assert_eq!(config.upstream.as_str(), DEFAULT_UPSTREAM, "{unset:?}");
            assert_eq!(config.timeout, DEFAULT_TIMEOUT);
            assert_eq!(config.max_in_flight, DEFAULT_MAX_IN_FLIGHT);
        }
        let config = JevRelayConfig::with_upstream_override(Some(" http://127.0.0.1:9/x "))
            .expect("http override");
        assert_eq!(config.upstream.as_str(), "http://127.0.0.1:9/x");
        for invalid in [
            "ftp://x",
            "not a url",
            "https://u:p@example.com/v1/systemone",
        ] {
            let error = JevRelayConfig::with_upstream_override(Some(invalid))
                .expect_err("invalid override");
            assert!(error.contains(UPSTREAM_ENV), "{error}");
        }
    }
}
