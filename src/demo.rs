//! `shrinkgate demo`: runs the gateway in-process against an in-process mock
//! Anthropic upstream and replays a 3-turn session with a large re-sent tool
//! result, printing per-request savings. Doubles as the integration check.

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::server::{serve, AppState};
use crate::store::Sessions;
use crate::upstream::make_client;

async fn mock_messages(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<crate::upstream::BoxBody>, hyper::Error> {
    let (_, body) = req.into_parts();
    let bytes = body.collect().await?.to_bytes();
    let n = (bytes.len() / 4) as i64;
    let sse = format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_mock\",\"role\":\"assistant\",\"content\":[],\"usage\":{{\"input_tokens\":{n},\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n\n\
         event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":9}}}}\n\n"
    );
    Ok(Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .body(crate::upstream::full(Bytes::from(sse)))
        .unwrap())
}

async fn spawn_mock() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(sock), service_fn(mock_messages))
                    .await;
            });
        }
    });
    port
}

fn tool_result(id: &str, text: &str) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": text})
}

pub async fn run() -> anyhow::Result<()> {
    let upstream_port = spawn_mock().await;
    let gateway_port = {
        let l = TcpListener::bind(("127.0.0.1", 0)).await?;
        let p = l.local_addr()?.port();
        drop(l); // free the port so the gateway can bind it below
        p
    };

    let cfg = Config {
        port: gateway_port,
        upstream_anthropic: format!("http://127.0.0.1:{upstream_port}"),
        ..Default::default()
    };
    let state_dir = std::env::temp_dir().join("shrinkgate-demo");
    let _ = std::fs::remove_dir_all(&state_dir);
    let state = Arc::new(AppState {
        clt: make_client(),
        sessions: Sessions::new(1024),
        state_dir: state_dir.clone(),
        cfg: cfg.clone(),
    });

    let listener = TcpListener::bind(("127.0.0.1", gateway_port)).await?;
    tokio::spawn(async move {
        let _ = serve(listener, state).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let clt = make_client();
    let big = "fn process_payment(amount: u64, currency: &str) -> Result<Receipt, PayError> {\n    let rate = fx_rate(currency, \"USD\")?;\n    let total = (amount as f64 * rate) as u64;\n    validate(total)?;\n    ledger.record(total, currency)?;\n    Ok(Receipt { total, currency: currency.to_string() })\n}\n".repeat(24);
    let system = json!([{"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}]);
    let meta = json!({"user_id": "demo-session-1"});

    let turns: Vec<(&str, Value)> = vec![
        (
            "turn 1: read payments.rs (fresh read)",
            json!({
                "model": "claude-sonnet-4-5", "max_tokens": 64, "stream": true,
                "system": system, "metadata": meta,
                "messages": [
                    {"role": "user", "content": [tool_result("t1", &big)]}
                ]
            }),
        ),
        (
            "turn 2: same history resent + a follow-up",
            json!({
                "model": "claude-sonnet-4-5", "max_tokens": 64, "stream": true,
                "system": system, "metadata": meta,
                "messages": [
                    {"role": "user", "content": [tool_result("t1", &big)]},
                    {"role": "assistant", "content": [{"type": "text", "text": "I'll fix the fx rate handling."}]},
                    {"role": "user", "content": [tool_result("t1", &big)]}
                ]
            }),
        ),
        (
            "turn 3: history resent again, file edited once",
            json!({
                "model": "claude-sonnet-4-5", "max_tokens": 64, "stream": true,
                "system": system, "metadata": meta,
                "messages": [
                    {"role": "user", "content": [tool_result("t1", &big)]},
                    {"role": "assistant", "content": [{"type": "text", "text": "I'll fix the fx rate handling."}]},
                    {"role": "user", "content": [tool_result("t1", &big)]},
                    {"role": "assistant", "content": [{"type": "text", "text": "Edited and re-read."}]},
                    {"role": "user", "content": [tool_result("t2", &big.replace("fx_rate", "fx_rate_v2"))]}
                ]
            }),
        ),
    ];

    println!("ShrinkGate demo — gateway :{gateway_port} → mock upstream :{upstream_port}\n");
    println!(
        "{:<44} {:>9} {:>9} {:>9} {:>9}",
        "request", "est.in", "sent.in", "saved", "upstream"
    );
    let mut total_saved = 0usize;
    for (label, body) in &turns {
        let req = hyper::Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{gateway_port}/v1/messages"))
            .header("content-type", "application/json")
            .header("x-api-key", "demo-key")
            .header("anthropic-version", "2023-06-01")
            .body(crate::upstream::full(Bytes::from(serde_json::to_vec(
                body,
            )?)))?;
        let resp = clt.request(req).await?;
        // Drain the SSE stream (gateway taps usage in parallel).
        let mut stream = resp.into_body().into_data_stream();
        while let Some(chunk) = stream.next().await {
            let _ = chunk?;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Read the latest journal record.
        let file = crate::paths::usage_file(&state_dir.to_path_buf());
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let last = text.lines().last().unwrap_or("{}");
        let rec: Value = serde_json::from_str(last)?;
        let est = rec.get("est_orig").and_then(Value::as_u64).unwrap_or(0);
        let sent = rec.get("est_sent").and_then(Value::as_u64).unwrap_or(0);
        let saved = rec.get("saved_tokens").and_then(Value::as_u64).unwrap_or(0);
        let upstream = rec
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        total_saved += saved as usize;
        println!(
            "{:<44} {:>9} {:>9} {:>9} {:>9}",
            label, est, sent, saved, upstream
        );
    }
    println!("\nTotal estimated savings this demo: −{total_saved} tokens (deterministic stubs, prompt-cache friendly).");
    println!(
        "Journal: {}",
        crate::paths::usage_file(&state_dir).display()
    );
    Ok(())
}
