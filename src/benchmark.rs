//! Reproducible local transformation measurements; never makes network calls.
use crate::{config::Config, shrink};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Instant};

pub(crate) fn fixture(format: &str, texts: &[String]) -> Value {
    match format {
        "anthropic" => {
            let mut messages = vec![json!({"role":"user","content":"Inspect the build output"})];
            for (i, text) in texts.iter().enumerate() {
                messages.push(json!({"role":"assistant","content":[{"type":"tool_use","id":format!("t{i}"),"name":"run","input":{"command":"cargo check"}}]}));
                messages.push(json!({"role":"user","content":[{"type":"tool_result","tool_use_id":format!("t{i}"),"content":text}]}));
            }
            json!({"model":"fixture-model","max_tokens":128,"messages":messages})
        }
        "chat" => {
            let mut messages = vec![json!({"role":"user","content":"Inspect the build output"})];
            for (i, text) in texts.iter().enumerate() {
                messages.push(json!({"role":"assistant","tool_calls":[{"id":format!("t{i}"),"type":"function","function":{"name":"run","arguments":"{}"}}]}));
                messages.push(json!({"role":"tool","tool_call_id":format!("t{i}"),"content":text}));
            }
            json!({"model":"fixture-model","messages":messages})
        }
        _ => {
            let mut input = vec![
                json!({"role":"user","content":[{"type":"input_text","text":"Inspect the build output"}]}),
            ];
            for (i, text) in texts.iter().enumerate() {
                input.push(json!({"type":"function_call","call_id":format!("t{i}"),"name":"run","arguments":"{}"}));
                input.push(
                    json!({"type":"function_call_output","call_id":format!("t{i}"),"output":text}),
                );
            }
            json!({"model":"fixture-model","input":input})
        }
    }
}

pub fn run(input: Option<PathBuf>, iterations: usize) -> anyhow::Result<()> {
    let fixtures = if let Some(path) = input {
        anyhow::ensure!(
            std::fs::metadata(&path)?.len() <= 16 << 20,
            "fixture exceeds 16 MiB"
        );
        vec![(
            "custom".to_string(),
            serde_json::from_slice(&std::fs::read(path)?)?,
        )]
    } else {
        let text = "error[E0308]: mismatched types in src/payments.rs:42\n".repeat(100);
        let scenarios = [
            ("repeated", vec![text.clone(), text.clone(), text.clone()]),
            (
                "unique",
                vec![
                    text.clone(),
                    text.replace("42", "43"),
                    text.replace("42", "44"),
                ],
            ),
            (
                "changed_tail",
                vec![text.clone(), text.clone(), text.replace("42", "43")],
            ),
            ("short", vec!["ok".into(), "ok".into(), "ok".into()]),
        ];
        let mut fixtures = Vec::new();
        for format in ["anthropic", "chat", "responses"] {
            for (name, texts) in &scenarios {
                fixtures.push((format!("{format}/{name}"), fixture(format, texts)));
            }
        }
        fixtures
    };
    let cfg = Config::default();
    let mut results = Vec::new();
    for (name, body) in fixtures {
        let format = if body.get("input").is_some() {
            "responses"
        } else if body.get("max_tokens").is_some() {
            "anthropic"
        } else {
            "chat"
        };
        anyhow::ensure!(
            body.get("input").is_some() || body.get("messages").is_some(),
            "fixture must contain input or messages"
        );
        let original = serde_json::to_vec(&body)?.len();
        let mut sent = 0;
        let mut blocks = 0;
        let mut micros = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let mut v = body.clone();
            let now = Instant::now();
            let stat = match format {
                "anthropic" => shrink::anthropic::transform(&mut v, &cfg),
                "responses" => shrink::responses::transform(&mut v, &cfg),
                _ => shrink::openai::transform(&mut v, &cfg),
            };
            let out = serde_json::to_vec(&v)?;
            micros.push(now.elapsed().as_micros());
            sent = out.len();
            blocks = stat.blocks_stubbed;
            anyhow::ensure!(sent <= original, "transform expanded fixture");
        }
        micros.sort_unstable();
        results.push(
            json!({"scenario":name,"request_bytes_before":original,"request_bytes_after":sent,
            "request_byte_reduction_percent":100.0 * (original - sent) as f64 / original as f64,
            "blocks_stubbed":blocks,"transform_and_serialize_p50_us":micros[iterations/2],
            "transform_and_serialize_p95_us":micros[(iterations-1)*95/100]}),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"kind":"local fixture benchmark; no API calls, token counts, billing or task-quality measurements", "version":env!("CARGO_PKG_VERSION"), "iterations":iterations, "configuration":"compiled defaults", "results":results})
        )?
    );
    Ok(())
}
