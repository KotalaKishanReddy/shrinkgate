//! The gateway server: localhost HTTP listener that sits between coding
//! agents and their model APIs. Transforms Anthropic Messages, OpenAI Chat
//! Completions and OpenAI Responses request bodies with the shrink pass, then
//! forwards to the configured upstream with the agent's own auth headers,
//! streaming responses back untouched while tapping usage.

use bytes::Bytes;
use http_body_util::BodyExt;
use http_body_util::Limited;
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::http::request::Parts;
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

use crate::config::Config;
use crate::paths;
use crate::shrink::{
    anthropic as sh_anthropic, openai as sh_openai, responses as sh_responses, ShrinkStat,
};
use crate::store::{usage_from_json, Journal, Record, Usage};
use crate::upstream::{
    forward_req_headers, forward_resp_headers, full, hyper_stream, make_client, BoxBody, BoxError,
    StreamOutcome, TapBody, UpstreamClient,
};

const MAX_BODY: usize = 16 << 20; // request bodies we transform
const MAX_RESP: usize = 64 << 20; // response bodies we buffer for usage taps

pub struct AppState {
    pub cfg: Config,
    pub clt: UpstreamClient,
    pub journal: Journal,
}

pub type St = Arc<AppState>;

pub async fn run(cfg: Config, quiet: bool) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = TcpListener::bind(addr).await?;
    let cfg_dir = paths::config_dir();
    Config::write_default_example(&cfg_dir);
    let state = Arc::new(AppState {
        clt: make_client(),
        journal: Journal::new(paths::state_dir())?,
        cfg: cfg.clone(),
    });
    if !quiet {
        println!(
            "ShrinkGate {} listening on http://127.0.0.1:{} (shrink: {}, Ctrl+C to stop)",
            env!("CARGO_PKG_VERSION"),
            cfg.port,
            cfg.shrink
        );
    }
    serve(listener, state).await
}

pub async fn serve(listener: TcpListener, state: St) -> anyhow::Result<()> {
    let sig = shutdown_signal();
    tokio::pin!(sig);
    let slots = Arc::new(Semaphore::new(16));
    let (stop, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut sig => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (sock, _) = accepted?;
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
                let st = state.clone();
                let mut stopped = stop.subscribe();
                connections.spawn(async move {
                    let _permit = permit;
                    let svc = service_fn(move |req| handle(req, st.clone()));
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder.timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(Duration::from_secs(15));
                    let connection = builder.serve_connection(TokioIo::new(sock), svc);
                    tokio::pin!(connection);
                    tokio::select! {
                        _ = &mut connection => {},
                        _ = stopped.changed() => {
                            connection.as_mut().graceful_shutdown();
                            let _ = connection.await;
                        }
                    }

                });
            }
        }
    }
    let _ = stop.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    if tokio::time::timeout(Duration::from_secs(5), state.journal.flush())
        .await
        .is_err()
    {
        eprintln!("shrinkgate: journal flush timed out; pending metadata may be lost");
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn handle(
    req: Request<Incoming>,
    st: St,
) -> Result<Response<BoxBody>, std::convert::Infallible> {
    use futures_util::FutureExt;
    let res = std::panic::AssertUnwindSafe(route(req, st))
        .catch_unwind()
        .await;
    match res {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(e)) => {
            let status = e
                .downcast_ref::<GatewayError>()
                .map(|e| e.status)
                .unwrap_or(502);
            Ok(error_json(status, &e.to_string()))
        }
        Err(_) => Ok(error_json(500, "internal gateway panic (isolated)")),
    }
}

#[derive(Debug)]
struct GatewayError {
    status: u16,
    message: &'static str,
}
impl std::fmt::Display for GatewayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for GatewayError {}
fn fail(status: u16, message: &'static str) -> BoxError {
    Box::new(GatewayError { status, message })
}

async fn read_body(body: Incoming, limit: usize, request: bool) -> Result<Bytes, BoxError> {
    let collected = tokio::time::timeout(
        Duration::from_secs(120),
        Limited::new(body, limit).collect(),
    )
    .await
    .map_err(|_| fail(if request { 408 } else { 504 }, "body read timed out"))?;
    collected.map(|b| b.to_bytes()).map_err(|_| {
        fail(
            if request { 413 } else { 502 },
            "body exceeded limit or could not be read",
        )
    })
}

fn error_json(status: u16, msg: &str) -> Response<BoxBody> {
    let body = json!({"error": {"type": "shrinkgate_error", "message": msg}});
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(full(serde_json::to_vec(&body).unwrap().into()))
        .unwrap()
}

async fn route(req: Request<Incoming>, st: St) -> Result<Response<BoxBody>, BoxError> {
    let (parts, body) = req.into_parts();
    // Browsers cannot use this credential-forwarding endpoint cross-origin.
    if parts.headers.contains_key("origin") {
        return Err(fail(403, "browser-origin requests are not allowed"));
    }
    let host = parts
        .headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if host != format!("127.0.0.1:{}", st.cfg.port) && host != format!("localhost:{}", st.cfg.port)
    {
        return Err(fail(403, "Host must match the local gateway address"));
    }
    if parts
        .headers
        .get("content-encoding")
        .is_some_and(|v| v != "identity")
    {
        return Err(fail(415, "compressed request bodies are not supported"));
    }
    if let Some(upstream) = force_upstream(&parts.headers) {
        if !["anthropic", "openai", "meta", "opencode"].contains(&upstream.as_str()) {
            return Err(fail(400, "unknown x-shrinkgate-upstream"));
        }
    }
    let path = parts.uri.path().to_string();
    let method = parts.method.clone();
    match (&method, path.as_str()) {
        (&Method::GET, "/healthz") | (&Method::GET, "/__cs/health") => Ok(health(&st)),
        (&Method::POST, "/v1/messages") => messages(parts, body, st, "messages").await,
        (&Method::POST, "/v1/messages/count_tokens") => {
            messages(parts, body, st, "count_tokens").await
        }
        (&Method::POST, "/v1/chat/completions") => chat_completions(parts, body, st).await,
        (&Method::POST, "/v1/responses") => responses(parts, body, st).await,
        _ => passthrough(parts, body, st).await,
    }
}

fn health(st: &St) -> Response<BoxBody> {
    let body = json!({
        "ok": true,
        "engine": "shrinkgate",
        "version": env!("CARGO_PKG_VERSION"),
        "shrink": st.cfg.shrink,
        "port": st.cfg.port,
        "health_protocol": 2,
        "config_fingerprint": st.cfg.fingerprint(),
        "journal_dropped_records": st.journal.dropped(),
    });
    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, "application/json")
        .body(full(serde_json::to_vec(&body).unwrap().into()))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Upstream helpers
// ---------------------------------------------------------------------------

fn path_and_query(parts: &Parts) -> String {
    let pq = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("");
    pq.to_string()
}

/// Strips a leading `/v1` segment (Codex ChatGPT-backend path mapping).
fn strip_v1(path: &str) -> &str {
    path.strip_prefix("/v1").unwrap_or(path)
}

fn force_upstream(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-shrinkgate-upstream")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
}

fn build_upstream_request(
    parts: &Parts,
    url: String,
    body: Bytes,
    identity_ae: bool,
) -> Result<Request<BoxBody>, BoxError> {
    let uri: Uri = url.parse().map_err(|e| Box::new(e) as BoxError)?;
    let headers = forward_req_headers(&parts.headers, identity_ae);
    let mut b = Request::builder().method(parts.method.clone()).uri(uri);
    if let Some(hm) = b.headers_mut() {
        for (k, v) in headers.iter() {
            hm.append(k.clone(), v.clone());
        }
        hm.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
    }
    b.body(full(body)).map_err(|e| Box::new(e) as BoxError)
}

/// Builds a passthrough request that streams the client's body onward.
fn build_stream_request(
    parts: &Parts,
    url: String,
    body: BoxBody,
) -> Result<Request<BoxBody>, BoxError> {
    let uri: Uri = url.parse().map_err(|e| Box::new(e) as BoxError)?;
    let headers = forward_req_headers(&parts.headers, false);
    let mut b = Request::builder().method(parts.method.clone()).uri(uri);
    if let Some(hm) = b.headers_mut() {
        for (k, v) in headers.iter() {
            hm.append(k.clone(), v.clone());
        }
    }
    b.body(body).map_err(|e| Box::new(e) as BoxError)
}

#[derive(Clone)]
struct Pending {
    route: &'static str,
    session: String,
    stat: ShrinkStat,
    started: Instant,
}

fn finish_with_usage(
    st: &St,
    p: &Pending,
    status: u16,
    usage: Option<Usage>,
    stream_outcome: Option<&'static str>,
) {
    let rec = Record {
        route: p.route,
        session: p.session.clone(),
        est_orig_bytes: p.stat.est_orig_bytes,
        est_sent_bytes: p.stat.est_sent_bytes,
        saved_bytes: p.stat.saved_bytes,
        blocks_stubbed: p.stat.blocks_stubbed,
        status,
        ms: p.started.elapsed().as_millis(),
        usage,
        stream_outcome,
    };
    st.journal.submit(rec);
}

/// Sends the transformed request upstream and returns the tap/forward response.
async fn send_transformed(
    st: &St,
    parts: Parts,
    url: String,
    body_bytes: Vec<u8>,
    pending: Pending,
) -> Result<Response<BoxBody>, BoxError> {
    let req = build_upstream_request(&parts, url, Bytes::from(body_bytes), true)?;
    let resp = tokio::time::timeout(Duration::from_secs(120), st.clt.request(req)).await
        .map_err(|_| fail(504, "upstream response timed out"))?.map_err(|e| {
        format!("ShrinkGate could not reach the upstream API: {e}. Your agent's auth headers are forwarded unchanged.")
    })?;
    let (up_parts, up_body) = resp.into_parts();
    let status = up_parts.status.as_u16();
    let is_sse = up_parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("event-stream"))
        .unwrap_or(false);
    let headers_out = forward_resp_headers(&up_parts.headers);

    if is_sse {
        // Stream through untouched; tap usage from SSE events.
        let usage_cell = Arc::new(std::sync::Mutex::new(None::<Usage>));
        let st2 = st.clone();
        let p2 = pending.clone();
        let on_end = Box::new(move |usage: Option<Usage>, outcome: StreamOutcome| {
            finish_with_usage(
                &st2,
                &p2,
                outcome.journal_status(status),
                usage,
                Some(outcome.as_str()),
            );
        });
        let body = TapBody::new(hyper_stream(up_body.into_data_stream()), usage_cell, on_end);
        let mut rb = Response::builder().status(status);
        if let Some(hm) = rb.headers_mut() {
            for (k, v) in headers_out.iter() {
                hm.append(k.clone(), v.clone());
            }
            hm.insert("x-shrinkgate", HeaderValue::from_static("streamed"));
        }
        rb.body(BoxBody::new(body))
            .map_err(|e| Box::new(e) as BoxError)
    } else {
        // Buffer (bounded), extract usage, journal, forward bytes.
        let bytes = read_body(up_body, MAX_RESP, false).await?;
        let usage = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| usage_from_json(&v));
        finish_with_usage(st, &pending, status, usage, None);
        let mut rb = Response::builder().status(status);
        if let Some(hm) = rb.headers_mut() {
            for (k, v) in headers_out.iter() {
                hm.append(k.clone(), v.clone());
            }
            hm.insert(CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        }
        rb.body(full(bytes)).map_err(|e| Box::new(e) as BoxError)
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

fn provider_base(st: &St, provider: &str) -> String {
    match provider {
        "anthropic" => st.cfg.upstream_anthropic.clone(),
        "meta" => st.cfg.upstream_meta.clone(),
        "opencode" => st.cfg.upstream_opencode.clone(),
        _ => st.cfg.upstream_openai.clone(),
    }
}

fn anthropic_base(st: &St, parts: &Parts) -> String {
    provider_base(
        st,
        force_upstream(&parts.headers)
            .as_deref()
            .unwrap_or("anthropic"),
    )
}

async fn messages(
    parts: Parts,
    body: Incoming,
    st: St,
    route: &'static str,
) -> Result<Response<BoxBody>, BoxError> {
    let bytes = read_body(body, MAX_BODY, true).await?;
    let mut v: Value =
        serde_json::from_slice(&bytes).map_err(|_| fail(400, "request body is not valid JSON"))?;
    let session = sh_anthropic::session_key(&v);
    let mut stat = ShrinkStat::default();
    if st.cfg.shrink {
        let cfg = st.cfg.clone();
        stat = sh_anthropic::transform(&mut v, &cfg);
    }
    let out = if st.cfg.shrink {
        serde_json::to_vec(&v)?
    } else {
        bytes.to_vec()
    };
    let url = format!(
        "{}{}",
        anthropic_base(&st, &parts).trim_end_matches('/'),
        path_and_query(&parts)
    );
    let pending = Pending {
        route,
        session,
        stat,
        started: Instant::now(),
    };
    send_transformed(&st, parts, url, out, pending).await
}

fn openai_base(st: &St, parts: &Parts) -> String {
    provider_base(
        st,
        force_upstream(&parts.headers)
            .as_deref()
            .unwrap_or(&st.cfg.default_upstream),
    )
}

async fn chat_completions(
    parts: Parts,
    body: Incoming,
    st: St,
) -> Result<Response<BoxBody>, BoxError> {
    let bytes = read_body(body, MAX_BODY, true).await?;
    let mut v: Value =
        serde_json::from_slice(&bytes).map_err(|_| fail(400, "request body is not valid JSON"))?;
    let session = sh_openai::session_key(&v);
    let mut stat = ShrinkStat::default();
    if st.cfg.shrink {
        let cfg = st.cfg.clone();
        stat = sh_openai::transform(&mut v, &cfg);
    }
    let out = if st.cfg.shrink {
        serde_json::to_vec(&v)?
    } else {
        bytes.to_vec()
    };
    let url = format!(
        "{}{}",
        openai_base(&st, &parts).trim_end_matches('/'),
        path_and_query(&parts)
    );
    let pending = Pending {
        route: "chat",
        session,
        stat,
        started: Instant::now(),
    };
    send_transformed(&st, parts, url, out, pending).await
}

/// Codex ChatGPT login supplies a `chatgpt-account-id` header; route those
/// to the ChatGPT backend with the `/v1` prefix stripped. Everything else goes
/// to the configured OpenAI-compatible upstream.
fn responses_upstream(st: &St, parts: &Parts) -> (String, String) {
    if force_upstream(&parts.headers).is_some() || st.cfg.default_upstream != "openai" {
        return (openai_base(st, parts), "explicit".into());
    }
    // Token shape is not provider identity. Only the account header selects
    // the ChatGPT backend; an explicit provider/default always takes precedence.
    let chatgpt = parts.headers.contains_key("chatgpt-account-id");
    if chatgpt {
        return (
            st.cfg.upstream_chatgpt.trim_end_matches('/').to_string(),
            "chatgpt".into(),
        );
    }
    (openai_base(st, parts), "openai".into())
}

async fn responses(parts: Parts, body: Incoming, st: St) -> Result<Response<BoxBody>, BoxError> {
    let bytes = read_body(body, MAX_BODY, true).await?;
    let mut v: Value =
        serde_json::from_slice(&bytes).map_err(|_| fail(400, "request body is not valid JSON"))?;
    let session = sh_responses::session_key(&v);
    let mut stat = ShrinkStat::default();
    if st.cfg.shrink {
        let cfg = st.cfg.clone();
        stat = sh_responses::transform(&mut v, &cfg);
    }
    let out = if st.cfg.shrink {
        serde_json::to_vec(&v)?
    } else {
        bytes.to_vec()
    };
    let (base, name) = responses_upstream(&st, &parts);
    let pq = path_and_query(&parts);
    let path = if name == "chatgpt" {
        strip_v1(&pq).to_string()
    } else {
        pq
    };
    let url = format!("{}{}", base, path);
    let pending = Pending {
        route: "responses",
        session,
        stat,
        started: Instant::now(),
    };
    send_transformed(&st, parts, url, out, pending).await
}

/// Anything else: pure reverse proxy to the guessed upstream.
async fn passthrough(parts: Parts, body: Incoming, st: St) -> Result<Response<BoxBody>, BoxError> {
    let path = parts.uri.path().to_string();
    let base = if let Some(provider) = force_upstream(&parts.headers) {
        provider_base(&st, &provider)
    } else if path.starts_with("/v1/messages") || path.starts_with("/v1/complete") {
        st.cfg.upstream_anthropic.clone()
    } else if st.cfg.default_upstream == "openai"
        && parts.headers.contains_key("chatgpt-account-id")
        && path.starts_with("/v1/")
    {
        st.cfg.upstream_chatgpt.clone()
    } else {
        provider_base(&st, &st.cfg.default_upstream)
    };
    let pq = path_and_query(&parts);
    let path = if base == st.cfg.upstream_chatgpt {
        strip_v1(&pq).to_string()
    } else {
        pq
    };
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let req = build_stream_request(&parts, url, hyper_stream(body.into_data_stream()))?;
    let resp = tokio::time::timeout(Duration::from_secs(120), st.clt.request(req)).await
        .map_err(|_| fail(504, "upstream response timed out"))?.map_err(|e| {
        format!("ShrinkGate could not reach the upstream API: {e}. Your agent's auth headers are forwarded unchanged.")
    })?;
    let (up_parts, up_body) = resp.into_parts();
    let headers_out = forward_resp_headers(&up_parts.headers);
    let mut rb = Response::builder().status(up_parts.status);
    if let Some(hm) = rb.headers_mut() {
        for (k, v) in headers_out.iter() {
            hm.append(k.clone(), v.clone());
        }
    }
    rb.body(hyper_stream(up_body.into_data_stream()))
        .map_err(|e| Box::new(e) as BoxError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::make_client;

    async fn gateway() -> (u16, tempfile::TempDir, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            port,
            ..Config::default()
        };
        let state = Arc::new(AppState {
            cfg,
            clt: make_client(),
            journal: Journal::new(dir.path().to_path_buf()).unwrap(),
        });
        let handle = tokio::spawn(async move {
            serve(listener, state).await.unwrap();
        });
        (port, dir, handle)
    }
    #[tokio::test]
    async fn invalid_json_compression_origin_and_size_return_client_errors() {
        let (port, _dir, task) = gateway().await;
        for (header, value, body, expected) in [
            (
                "content-type",
                "application/json",
                Bytes::from_static(b"{broken"),
                400,
            ),
            ("content-encoding", "gzip", Bytes::from_static(b"{}"), 415),
            (
                "origin",
                "https://evil.test",
                Bytes::from_static(b"{}"),
                403,
            ),
            ("host", "evil.test", Bytes::from_static(b"{}"), 403),
            (
                "content-type",
                "application/json",
                Bytes::from(vec![b'x'; MAX_BODY + 1]),
                413,
            ),
        ] {
            let req = Request::builder()
                .method("POST")
                .uri(format!("http://127.0.0.1:{port}/v1/messages"))
                .header(header, value)
                .body(full(body))
                .unwrap();
            let response = make_client().request(req).await.unwrap();
            assert_eq!(response.status().as_u16(), expected);
        }
        task.abort();
    }
    #[test]
    fn explicit_upstream_overrides_chatgpt_account_routing() {
        let cfg = Config::default();
        let temp = tempfile::tempdir().unwrap();
        let st = Arc::new(AppState {
            cfg: cfg.clone(),
            clt: make_client(),
            journal: Journal::new(temp.path().to_path_buf()).unwrap(),
        });
        let (parts, _) = Request::builder()
            .header("authorization", "Bearer eyJtest")
            .header("chatgpt-account-id", "fixture-account")
            .header("x-shrinkgate-upstream", "meta")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(responses_upstream(&st, &parts).0, cfg.upstream_meta);
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use crate::benchmark::fixture;
    #[tokio::test]
    async fn all_protocols_transform_and_stream_identical_response_bytes() {
        let captures = Arc::new(std::sync::Mutex::new(Vec::new()));
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_url = format!("http://127.0.0.1:{}", upstream.local_addr().unwrap().port());
        let recorded = captures.clone();
        let mock = tokio::spawn(async move {
            loop {
                let (socket, _) = upstream.accept().await.unwrap();
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<Incoming>| {
                        let recorded = recorded.clone();
                        async move {
                            assert_eq!(req.headers()["authorization"], "Bearer fixture-key");
                            assert!(!req.headers().contains_key("x-shrinkgate-upstream"));
                            let path = req.uri().path().to_string();
                            let bytes = req.into_body().collect().await.unwrap().to_bytes();
                            let v: Value = serde_json::from_slice(&bytes).unwrap();
                            recorded.lock().unwrap().push((path, v));
                            let chunks = futures_util::stream::iter([
                                Ok(Bytes::from_static(b"data: {\"usage\":{\"input_")),
                                Ok(Bytes::from_static(
                                    b"tokens\":123,\"output_tokens\":7}}\n\ndata: [DONE]\n\n",
                                )),
                            ]);
                            Ok::<_, std::convert::Infallible>(
                                Response::builder()
                                    .header(CONTENT_TYPE, "text/event-stream")
                                    .body(crate::upstream::boxed_stream(chunks))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            port,
            upstream_anthropic: upstream_url.clone(),
            upstream_openai: upstream_url,
            ..Config::default()
        };
        let state = Arc::new(AppState {
            cfg,
            clt: make_client(),
            journal: Journal::new(dir.path().to_path_buf()).unwrap(),
        });
        let engine = tokio::spawn(async move {
            serve(listener, state).await.unwrap();
        });
        let text = "file content".repeat(300);
        for (format, path) in [
            ("anthropic", "/v1/messages"),
            ("chat", "/v1/chat/completions"),
            ("responses", "/v1/responses"),
        ] {
            let v = fixture(format, &[text.clone(), text.clone(), text.clone()]);
            let req = Request::builder()
                .method("POST")
                .uri(format!("http://127.0.0.1:{port}{path}"))
                .header("authorization", "Bearer fixture-key")
                .header(CONTENT_TYPE, "application/json")
                .body(full(serde_json::to_vec(&v).unwrap().into()))
                .unwrap();
            let response = make_client().request(req).await.unwrap();
            assert_eq!(response.status(), 200);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                &bytes[..],
                b"data: {\"usage\":{\"input_tokens\":123,\"output_tokens\":7}}\n\ndata: [DONE]\n\n"
            );
        }
        let captured = captures.lock().unwrap();
        assert_eq!(captured.len(), 3);
        assert!(captured[0].1["messages"][2]["content"][0]["content"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
        assert_eq!(captured[0].1["messages"][6]["content"][0]["content"], text);
        assert!(captured[1].1["messages"][2]["content"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
        assert!(captured[2].1["input"][2]["output"]
            .as_str()
            .unwrap()
            .starts_with("[shrinkgate]"));
        drop(captured);
        engine.abort();
        mock.abort();
    }
}

#[cfg(test)]
mod audit_regressions {
    use super::*;
    use crate::benchmark::fixture;
    use std::sync::Mutex;

    async fn backend(
        body: &'static [u8],
        hits: Arc<Mutex<Vec<(String, String)>>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let task = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let hits = hits.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<Incoming>| {
                        let hits = hits.clone();
                        async move {
                            hits.lock().unwrap().push((
                                req.uri().path().into(),
                                req.headers()
                                    .get("authorization")
                                    .unwrap()
                                    .to_str()
                                    .unwrap()
                                    .into(),
                            ));
                            let _ = req.into_body().collect().await.unwrap();
                            Ok::<_, std::convert::Infallible>(
                                Response::builder()
                                    .header(CONTENT_TYPE, "text/event-stream")
                                    .header("connection", "close")
                                    .body(full(Bytes::from_static(body)))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        (base, task)
    }

    async fn request(port: u16, path: &str, provider: Option<&str>, body: Value) -> (u16, Bytes) {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{port}{path}"))
            .header("authorization", "Bearer eyJ-local-fixture")
            .header(CONTENT_TYPE, "application/json");
        if let Some(provider) = provider {
            builder = builder.header("x-shrinkgate-upstream", provider);
        }
        let response = make_client()
            .request(
                builder
                    .body(full(serde_json::to_vec(&body).unwrap().into()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        (
            status,
            response.into_body().collect().await.unwrap().to_bytes(),
        )
    }

    #[tokio::test]
    async fn meta_default_and_explicit_overrides_never_leak_auth_to_other_backend() {
        let openai_hits = Arc::new(Mutex::new(Vec::new()));
        let meta_hits = Arc::new(Mutex::new(Vec::new()));
        let (openai, openai_task) = backend(b"data: [DONE]\n\n", openai_hits.clone()).await;
        let (meta, meta_task) = backend(b"data: [DONE]\n\n", meta_hits.clone()).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            port,
            default_upstream: "meta".into(),
            upstream_openai: openai,
            upstream_meta: meta,
            ..Config::default()
        };
        let state = Arc::new(AppState {
            cfg,
            clt: make_client(),
            journal: Journal::new(dir.path().to_path_buf()).unwrap(),
        });
        let engine_state = state.clone();
        let task = tokio::spawn(async move {
            serve(listener, engine_state).await.unwrap();
        });
        for path in ["/v1/responses", "/v1/chat/completions", "/v1/models"] {
            assert_eq!(
                request(
                    port,
                    path,
                    None,
                    json!({"model":"fixture","input":[],"messages":[]})
                )
                .await
                .0,
                200
            );
        }
        assert_eq!(meta_hits.lock().unwrap().len(), 3);
        assert!(openai_hits.lock().unwrap().is_empty());
        assert_eq!(
            request(port, "/v1/responses", Some("openai"), json!({"input":[]}))
                .await
                .0,
            200
        );
        assert_eq!(openai_hits.lock().unwrap().len(), 1);
        assert!(meta_hits
            .lock()
            .unwrap()
            .iter()
            .all(|(_, auth)| auth == "Bearer eyJ-local-fixture"));
        state.journal.flush().await;
        task.abort();
        openai_task.abort();
        meta_task.abort();
    }

    #[tokio::test]
    async fn streamed_api_failure_is_forwarded_unchanged_and_never_earns_credit() {
        const BODY: &[u8] = b"data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":123}}}\n\nevent: error\ndata: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n\n";
        let (upstream, mock) = backend(BODY, Arc::new(Mutex::new(Vec::new()))).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            port,
            upstream_anthropic: upstream,
            ..Config::default()
        };
        let state = Arc::new(AppState {
            cfg,
            clt: make_client(),
            journal: Journal::new(dir.path().to_path_buf()).unwrap(),
        });
        let st = state.clone();
        let engine = tokio::spawn(async move {
            serve(listener, st).await.unwrap();
        });
        let text = "compiler output".repeat(200);
        let (status, bytes) = request(
            port,
            "/v1/messages",
            None,
            fixture("anthropic", &[text.clone(), text.clone(), text]),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(&bytes[..], BODY);
        state.journal.flush().await;
        let report = crate::store::summary(dir.path());
        assert_eq!(report["successful_requests"], 0);
        assert_eq!(report["estimated_saved_tokens"], 0);
        assert_eq!(report["failed_or_cancelled_requests"], 1);
        assert_eq!(
            report["provider_usage"]["failed_or_cancelled"]["input_tokens"],
            123
        );
        engine.abort();
        mock.abort();
    }
}

#[cfg(test)]
mod provider_identity_regression {
    use super::*;
    #[test]
    fn jwt_shape_is_not_an_instruction_to_change_provider() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            upstream_openai: "http://127.0.0.1:12345".into(),
            ..Config::default()
        };
        let state = Arc::new(AppState {
            cfg: cfg.clone(),
            clt: make_client(),
            journal: Journal::new(dir.path().to_path_buf()).unwrap(),
        });
        let (parts, _) = Request::builder()
            .header("authorization", "Bearer eyJ-provider-token")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(responses_upstream(&state, &parts).0, cfg.upstream_openai);
    }
}
