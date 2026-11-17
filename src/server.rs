//! The gateway server: localhost HTTP listener that sits between coding
//! agents and their model APIs. Transforms Anthropic Messages, OpenAI Chat
//! Completions and OpenAI Responses request bodies with the shrink pass, then
//! forwards to the configured upstream with the agent's own auth headers,
//! streaming responses back untouched while tapping usage.

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::http::request::Parts;
use hyper::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, Uri};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::paths;
use crate::shrink::{anthropic as sh_anthropic, openai as sh_openai, responses as sh_responses, ShrinkStat};
use crate::store::{journal, usage_from_json, Record, Sessions, Usage};
use crate::upstream::{
    forward_req_headers, forward_resp_headers, full, hyper_stream, make_client, BoxBody, BoxError,
    TapBody, UpstreamClient,
};

const MAX_BODY: usize = 256 << 20; // request bodies we transform
const MAX_RESP: usize = 64 << 20; // response bodies we buffer for usage taps

pub struct AppState {
    pub cfg: Config,
    pub clt: UpstreamClient,
    pub sessions: Sessions,
    pub state_dir: PathBuf,
}

pub type St = Arc<AppState>;

pub async fn run(cfg: Config, quiet: bool) -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.port));
    let listener = TcpListener::bind(addr).await?;
    let cfg_dir = paths::config_dir();
    Config::write_default_example(&cfg_dir);
    let state = Arc::new(AppState {
        clt: make_client(),
        sessions: Sessions::new(cfg.max_sessions),
        state_dir: paths::state_dir(),
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
    let sig = tokio::signal::ctrl_c();
    tokio::pin!(sig);
    loop {
        tokio::select! {
            _ = &mut sig => break,
            accepted = listener.accept() => {
                let (sock, _) = accepted?;
                let st = state.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req| handle(req, st.clone()));
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(sock), svc)
                        .await;
                });
            }
        }
    }
    Ok(())
}

async fn handle(req: Request<Incoming>, st: St) -> Result<Response<BoxBody>, std::convert::Infallible> {
    Ok(route(req, st).await.unwrap_or_else(|e| error_json(502, &e.to_string())))
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
    let path = parts.uri.path().to_string();
    let method = parts.method.clone();
    match (&method, path.as_str()) {
        (&Method::GET, "/healthz") | (&Method::GET, "/__cs/health") => Ok(health(&st)),
        (&Method::POST, "/v1/messages") => messages(parts, body, st, "messages").await,
        (&Method::POST, "/v1/messages/count_tokens") => messages(parts, body, st, "count_tokens").await,
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
    let pq = parts.uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("");
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
fn build_stream_request(parts: &Parts, url: String, body: BoxBody) -> Result<Request<BoxBody>, BoxError> {
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

fn finish_with_usage(st: &St, p: &Pending, status: u16, usage: Option<Usage>) {
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
    };
    journal(&st.state_dir, &rec);
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
    let started_body = Instant::now();
    let resp = st.clt.request(req).await.map_err(|e| {
        format!("ShrinkGate could not reach the upstream API: {e}. Your agent's auth headers are forwarded unchanged.")
    })?;
    let _ = started_body;
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
        let on_end = Box::new(move |usage: Option<Usage>| {
            finish_with_usage(&st2, &p2, status, usage);
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
        let bytes = up_body.collect().await?.to_bytes();
        if bytes.len() > MAX_RESP {
            return Err("upstream response exceeded the ShrinkGate buffer".into());
        }
        let usage = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| usage_from_json(&v));
        finish_with_usage(st, &pending, status, usage);
        let mut rb = Response::builder().status(status);
        if let Some(hm) = rb.headers_mut() {
            for (k, v) in headers_out.iter() {
                hm.append(k.clone(), v.clone());
            }
            hm.insert(CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        }
        rb.body(full(bytes))
            .map_err(|e| Box::new(e) as BoxError)
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

fn anthropic_base(st: &St, parts: &Parts) -> String {
    match force_upstream(&parts.headers).as_deref() {
        Some("openai") => st.cfg.upstream_openai.clone(),
        Some("meta") => st.cfg.upstream_meta.clone(),
        _ => st.cfg.upstream_anthropic.clone(),
    }
}

async fn messages(parts: Parts, body: Incoming, st: St, route: &'static str) -> Result<Response<BoxBody>, BoxError> {
    let bytes = body.collect().await?.to_bytes();
    if bytes.len() > MAX_BODY {
        return Err("request body exceeded the ShrinkGate limit".into());
    }
    let mut v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("request body is not valid JSON: {e}"))?;
    let session = sh_anthropic::session_key(&v);
    let mut stat = ShrinkStat::default();
    if st.cfg.shrink {
        let cfg = st.cfg.clone();
        stat = st
            .sessions
            .with_session(&session, |sess| sh_anthropic::transform(&mut v, sess, &cfg));
    }
    let out = serde_json::to_vec(&v)?;
    let url = format!("{}{}", anthropic_base(&st, &parts).trim_end_matches('/'), path_and_query(&parts));
    let pending = Pending { route, session, stat, started: Instant::now() };
    send_transformed(&st, parts, url, out, pending).await
}

fn openai_base(st: &St, parts: &Parts) -> String {
    match force_upstream(&parts.headers).as_deref() {
        Some("anthropic") => st.cfg.upstream_anthropic.clone(),
        Some("meta") => st.cfg.upstream_meta.clone(),
        Some("opencode") => st.cfg.upstream_opencode.clone(),
        _ => st.cfg.upstream_openai.clone(),
    }
}

async fn chat_completions(parts: Parts, body: Incoming, st: St) -> Result<Response<BoxBody>, BoxError> {
    let bytes = body.collect().await?.to_bytes();
    if bytes.len() > MAX_BODY {
        return Err("request body exceeded the ShrinkGate limit".into());
    }
    let mut v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("request body is not valid JSON: {e}"))?;
    let session = sh_openai::session_key(&v);
    let mut stat = ShrinkStat::default();
    if st.cfg.shrink {
        let cfg = st.cfg.clone();
        stat = st
            .sessions
            .with_session(&session, |sess| sh_openai::transform(&mut v, sess, &cfg));
    }
    let out = serde_json::to_vec(&v)?;
    let url = format!("{}{}", openai_base(&st, &parts).trim_end_matches('/'), path_and_query(&parts));
    let pending = Pending { route: "chat", session, stat, started: Instant::now() };
    send_transformed(&st, parts, url, out, pending).await
}

/// Codex ChatGPT login rides a JWT + `chatgpt-account-id` header; route those
/// to the ChatGPT backend with the `/v1` prefix stripped. Everything else goes
/// to the configured OpenAI-compatible upstream.
fn responses_upstream(st: &St, parts: &Parts) -> (String, String) {
    let auth = parts
        .headers
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let jwt = auth
        .strip_prefix("Bearer ")
        .map(|t| t.starts_with("eyJ"))
        .unwrap_or(false);
    let chatgpt = parts.headers.contains_key("chatgpt-account-id") || jwt;
    if chatgpt {
        return (st.cfg.upstream_chatgpt.trim_end_matches('/').to_string(), "chatgpt".into());
    }
    (openai_base(st, parts), "openai".into())
}

async fn responses(parts: Parts, body: Incoming, st: St) -> Result<Response<BoxBody>, BoxError> {
    let bytes = body.collect().await?.to_bytes();
    if bytes.len() > MAX_BODY {
        return Err("request body exceeded the ShrinkGate limit".into());
    }
    let mut v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("request body is not valid JSON: {e}"))?;
    let session = sh_responses::session_key(&v);
    let mut stat = ShrinkStat::default();
    if st.cfg.shrink {
        let cfg = st.cfg.clone();
        stat = st
            .sessions
            .with_session(&session, |sess| sh_responses::transform(&mut v, sess, &cfg));
    }
    let out = serde_json::to_vec(&v)?;
    let (base, name) = responses_upstream(&st, &parts);
    let pq = path_and_query(&parts);
    let path = if name == "chatgpt" { strip_v1(&pq).to_string() } else { pq };
    let url = format!("{}{}", base, path);
    let pending = Pending { route: "responses", session, stat, started: Instant::now() };
    send_transformed(&st, parts, url, out, pending).await
}

/// Anything else: pure reverse proxy to the guessed upstream.
async fn passthrough(parts: Parts, body: Incoming, st: St) -> Result<Response<BoxBody>, BoxError> {
    let path = parts.uri.path().to_string();
    let base = if force_upstream(&parts.headers).as_deref() == Some("meta") {
        st.cfg.upstream_meta.clone()
    } else if force_upstream(&parts.headers).as_deref() == Some("opencode") {
        st.cfg.upstream_opencode.clone()
    } else if path.starts_with("/v1/messages") || path.starts_with("/v1/complete") {
        anthropic_base(&st, &parts)
    } else if parts
        .headers
        .contains_key("chatgpt-account-id")
        && path.starts_with("/v1/")
    {
        st.cfg.upstream_chatgpt.clone()
    } else {
        match st.cfg.default_upstream.as_str() {
            "anthropic" => st.cfg.upstream_anthropic.clone(),
            "meta" => st.cfg.upstream_meta.clone(),
            "opencode" => st.cfg.upstream_opencode.clone(),
            _ => st.cfg.upstream_openai.clone(),
        }
    };
    let pq = path_and_query(&parts);
    let path = if base == st.cfg.upstream_chatgpt { strip_v1(&pq).to_string() } else { pq };
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let req = build_stream_request(&parts, url, hyper_stream(body.into_data_stream()))?;
    let resp = st.clt.request(req).await.map_err(|e| {
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
