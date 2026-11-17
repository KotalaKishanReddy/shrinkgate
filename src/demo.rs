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
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let gateway_port = listener.local_addr()?.port();
    let cfg = Config {
        port: gateway_port,
        upstream_anthropic: format!("http://127.0.0.1:{upstream_port}"),
        ..Default::default()
    };
    let temp = tempfile::tempdir()?;
    let state_dir = temp.path().to_path_buf();
    let state = Arc::new(AppState {
        clt: make_client(),
        state_dir: state_dir.clone(),
        cfg: cfg.clone(),
    });

    tokio::spawn(async move {
        let _ = serve(listener, state).await;
    });

    let clt = make_client();
    let big = "fn process_payment(amount: u64, currency: &str) -> Result<Receipt, PayError> {\n    let rate = fx_rate(currency, \"USD\")?;\n    let total = (amount as f64 * rate) as u64;\n    validate(total)?;\n    ledger.record(total, currency)?;\n    Ok(Receipt { total, currency: currency.to_string() })\n}\n".repeat(24);
    let system = json!([{"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}]);
    let meta = json!({"user_id": "demo-session-1"});

    let call = |id: &str| json!({"role": "assistant", "content": [{"type":"tool_use", "id": id, "name":"read_file", "input":{"path":"payments.rs"}}]});
    let mut history = vec![
        json!({"role":"user", "content":"Inspect payments.rs"}),
        call("t1"),
        json!({"role":"user", "content":[tool_result("t1", &big)]}),
    ];
    let mut turns = Vec::new();
    for (label, id, text) in [
        ("turn 1: fresh read", "t1", big.clone()),
        ("turn 2: repeated read", "t2", big.clone()),
        (
            "turn 3: changed file",
            "t3",
            big.replace("fx_rate", "fx_rate_v2"),
        ),
    ] {
        if id != "t1" {
            history.push(call(id));
            history.push(json!({"role":"user", "content":[tool_result(id, &text)]}));
        }
        turns.push((
            label,
            json!({"model":"fixture-model", "max_tokens":64, "stream":true,
            "system":system, "metadata":meta, "messages":history.clone()}),
        ));
    }

    println!("ShrinkGate demo — gateway :{gateway_port} → mock upstream :{upstream_port}\n");
    println!(
        "{:<44} {:>9} {:>9} {:>9} {:>9}",
        "request", "est.in", "sent.in", "saved", "upstream"
    );
    let mut total_saved = 0usize;
    for (index, (label, body)) in turns.iter().enumerate() {
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
        anyhow::ensure!(
            resp.status().is_success(),
            "demo request failed: {}",
            resp.status()
        );
        // Drain the SSE stream (gateway taps usage in parallel).
        let mut stream = resp.into_body().into_data_stream();
        while let Some(chunk) = stream.next().await {
            let _ = chunk?;
        }
        // Journaling runs off the network executor; wait for this request's record.
        let file = crate::paths::usage_file(&state_dir);
        let rec: Value = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                if let Some(record) = complete_record(&text, index + 1) {
                    break record;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await?;
        anyhow::ensure!(rec["status"] == 200, "demo stream failed");
        let blocks = rec["blocks"].as_u64().unwrap_or(0);
        anyhow::ensure!(
            blocks == if index == 0 { 0 } else { 1 },
            "unexpected duplicate count: {blocks}"
        );
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
    println!("\nTotal estimated savings this demo: −{total_saved} tokens (bytes/4 estimate; synthetic fixture, not measured billing savings).");
    println!(
        "Journal: {}",
        crate::paths::usage_file(&state_dir).display()
    );
    Ok(())
}

// A reader may observe a record between the writer's append calls.
fn complete_record(text: &str, expected: usize) -> Option<Value> {
    if !text.ends_with('\n') || text.lines().count() != expected {
        return None;
    }
    serde_json::from_str(text.lines().last()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waits_for_a_complete_valid_journal_line() {
        assert!(complete_record("{\"status\":", 1).is_none());
        assert!(complete_record("{\"status\":200}", 1).is_none());
        assert!(complete_record("{broken}\n", 1).is_none());
        assert!(complete_record("{}\n", 2).is_none());
        assert_eq!(
            complete_record("{\"status\":200}\n", 1).unwrap()["status"],
            200
        );
    }
}
