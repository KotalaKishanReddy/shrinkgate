//! Per-session shrink state and the local usage journal.

use ring::digest::{Context, SHA256};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Stable content fingerprint (hex, 64 chars). Length-prefixed parts avoid
/// ambiguity between concatenations.
pub fn fingerprint(parts: &[&[u8]]) -> String {
    let mut h = Context::new(&SHA256);
    for p in parts {
        let mut len = [0u8; 8];
        len.copy_from_slice(&(p.len() as u64).to_le_bytes());
        h.update(&len);
        h.update(p);
    }
    let digest = h.finish();
    hex_digest(digest.as_ref())
}

/// Lowercase hex of the full digest (64 chars).
fn hex_digest(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for byte in b {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0xf) as usize] as char);
    }
    s
}

/// Crude token estimate (≈ chars/4 for code and prose).
pub fn est_tokens(bytes: usize) -> usize {
    bytes / 4
}

// ---------------------------------------------------------------------------
// Usage extraction (Anthropic Messages, OpenAI Chat Completions, Responses API)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_creation: Option<u64>,
    pub output_tokens: Option<u64>,
}

fn get_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

impl Usage {
    /// Merges usage fields found anywhere in the shapes the three supported
    /// APIs use. Call with any JSON object; harmless when nothing matches.
    pub fn merge_json(&mut self, v: &Value) {
        let candidates = [
            Some(v),
            v.get("usage"),
            v.pointer("/message/usage"),
            v.pointer("/response/usage"),
        ];
        for c in candidates.into_iter().flatten() {
            if !c.is_object() {
                continue;
            }
            // Input tokens: anthropic + responses use input_tokens, chat uses prompt_tokens.
            if let Some(n) = get_u64(c, "input_tokens").or_else(|| get_u64(c, "prompt_tokens")) {
                self.input_tokens = Some(n);
            }
            if let Some(n) = get_u64(c, "output_tokens").or_else(|| get_u64(c, "completion_tokens"))
            {
                self.output_tokens = Some(n);
            }
            if let Some(n) = get_u64(c, "cache_read_input_tokens") {
                self.cache_read = Some(n);
            }
            if let Some(n) = get_u64(c, "cache_creation_input_tokens") {
                self.cache_creation = Some(n);
            }
            for details_key in ["prompt_tokens_details", "input_tokens_details"] {
                if let Some(n) = c
                    .get(details_key)
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(Value::as_u64)
                {
                    self.cache_read = Some(n);
                }
            }
        }
    }
}

pub fn usage_from_json(v: &Value) -> Option<Usage> {
    let mut u = Usage::default();
    u.merge_json(v);
    if u.input_tokens.is_some() || u.output_tokens.is_some() {
        Some(u)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Journal
// ---------------------------------------------------------------------------

pub struct Record {
    pub route: &'static str,
    pub session: String,
    pub est_orig_bytes: usize,
    pub est_sent_bytes: usize,
    pub saved_bytes: usize,
    pub blocks_stubbed: usize,
    pub status: u16,
    pub ms: u128,
    pub usage: Option<Usage>,
    pub stream_outcome: Option<&'static str>,
}

impl Record {
    pub fn to_json(&self) -> Value {
        json!({
            "ts": now_ms(),
            "route": self.route,
            "session": self.session,
            "est_orig": est_tokens(self.est_orig_bytes),
            "est_sent": est_tokens(self.est_sent_bytes),
            "saved_bytes": self.saved_bytes,
            "saved_tokens": est_tokens(self.saved_bytes),
            "blocks": self.blocks_stubbed,
            "status": self.status,
            "stream_outcome": self.stream_outcome,
            "ms": self.ms,
            "usage": self.usage.map(|u| json!({
                "input_tokens": u.input_tokens,
                "cache_read": u.cache_read,
                "cache_creation": u.cache_creation,
                "output_tokens": u.output_tokens,
            })),
        })
    }
}

const JOURNAL_QUEUE_CAPACITY: usize = 256;
enum JournalCommand {
    Record(Record),
    Flush(oneshot::Sender<()>),
}

/// One disk worker and a bounded queue. Overflow never blocks API responses:
/// it drops new metadata, increments a health counter and reports the loss.
pub struct Journal {
    tx: mpsc::Sender<JournalCommand>,
    dropped: Arc<AtomicU64>,
}
impl Journal {
    pub fn new(dir: std::path::PathBuf) -> anyhow::Result<Self> {
        Self::with_writer(JOURNAL_QUEUE_CAPACITY, move |record| journal(&dir, &record))
    }
    fn with_writer(
        capacity: usize,
        writer: impl Fn(Record) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (tx, mut rx) = mpsc::channel(capacity);
        std::thread::Builder::new()
            .name("shrinkgate-journal".into())
            .spawn(move || {
                while let Some(command) = rx.blocking_recv() {
                    match command {
                        JournalCommand::Record(record) => writer(record),
                        JournalCommand::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        Ok(Self {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
        })
    }
    pub fn submit(&self, record: Record) {
        if self.tx.try_send(JournalCommand::Record(record)).is_err() {
            let count = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if count.is_power_of_two() {
                eprintln!("shrinkgate: usage journal queue unavailable/full; {count} metadata records dropped (API traffic continues)");
            }
        }
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    pub async fn flush(&self) {
        let (done, completed) = oneshot::channel();
        if self.tx.send(JournalCommand::Flush(done)).await.is_ok() {
            let _ = completed.await;
        }
    }
}

static JOURNAL_LOCK: Mutex<()> = Mutex::new(());
const JOURNAL_LIMIT: u64 = 8 << 20;

pub fn journal(state_dir: &Path, rec: &Record) {
    let Ok(_lock) = JOURNAL_LOCK.lock() else {
        return;
    };
    let result = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let file = crate::paths::usage_file(state_dir);
        if std::fs::metadata(&file).is_ok_and(|m| m.len() >= JOURNAL_LIMIT) {
            let old = state_dir.join("usage.previous.jsonl");
            if old.exists() {
                std::fs::remove_file(&old)?;
            }
            std::fs::rename(&file, old)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(file)?;
        let line = rec.to_json().to_string() + "\n";
        f.write_all(line.as_bytes())
    })();
    if let Err(e) = result {
        eprintln!("shrinkgate: could not write usage metadata: {e}");
    }
}

/// Retained-journal totals, streamed from disk. Estimates are only credited to
/// completed 2xx requests. Failed/cancelled attempts are reported separately.
pub fn summary(state_dir: &Path) -> Value {
    let mut requests = 0u64;
    let mut failed = 0u64;
    let mut saved = 0u64;
    let mut today_saved = 0u64;
    let mut today_requests = 0u64;
    let mut input = 0u64;
    let mut output = 0u64;
    let mut cached = 0u64;
    let mut cache_write = 0u64;
    let mut measured = 0u64;
    let mut failed_usage_requests = 0u64;
    let mut failed_input = 0u64;
    let mut failed_output = 0u64;
    let mut failed_cached = 0u64;
    let mut failed_cache_write = 0u64;
    let day_start = now_ms() / 86_400_000 * 86_400_000;
    for path in [
        state_dir.join("usage.previous.jsonl"),
        crate::paths::usage_file(state_dir),
    ] {
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let num = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0);
            requests += 1;
            let success = (200..300).contains(&num("status"));
            if let Some(usage) = v.get("usage").filter(|u| u.is_object()) {
                measured += 1;
                let read = |key: &str| usage[key].as_u64().unwrap_or(0);
                input += read("input_tokens");
                output += read("output_tokens");
                cached += read("cache_read");
                cache_write += read("cache_creation");
                if !success {
                    failed_usage_requests += 1;
                    failed_input += read("input_tokens");
                    failed_output += read("output_tokens");
                    failed_cached += read("cache_read");
                    failed_cache_write += read("cache_creation");
                }
            }
            if !success {
                failed += 1;
                continue;
            }
            saved = saved.saturating_add(num("saved_tokens"));
            if num("ts") >= day_start {
                today_saved += num("saved_tokens");
                today_requests += 1;
            }
        }
    }
    json!({"scope": "retained local journals (up to approximately 16 MiB)",
        "estimator": "candidate UTF-8 bytes / 4; not billed savings", "day_timezone": "UTC",
        "requests": requests, "failed_or_cancelled_requests": failed, "successful_requests": requests - failed,
        "estimated_saved_tokens": saved, "today_estimated_saved_tokens": today_saved,
        "today_successful_requests": today_requests,
        "provider_usage": {"requests_with_usage": measured, "input_tokens": input, "output_tokens": output,
            "cache_read_tokens": cached, "cache_creation_tokens": cache_write,
            "scope": "all observed usage, including failed/cancelled requests; not an invoice",
            "failed_or_cancelled": {"requests_with_usage": failed_usage_requests, "input_tokens": failed_input,
                "output_tokens": failed_output, "cache_read_tokens": failed_cached, "cache_creation_tokens": failed_cache_write}}})
}

/// (today estimated reductions, today successful requests, retained reductions, retained successful requests).
pub fn totals(state_dir: &Path) -> (u64, u64, u64, u64) {
    let v = summary(state_dir);
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    (
        n("today_estimated_saved_tokens"),
        n("today_successful_requests"),
        n("estimated_saved_tokens"),
        n("successful_requests"),
    )
}

/// UTC calendar date (y, m, d) from a unix millisecond timestamp.
pub fn utc_ymd(ts_ms: u64) -> (i64, u32, u32) {
    let days = (ts_ms / 86_400_000) as i64;
    let l = days + 719_468;
    let era = if l >= 0 { l } else { l - 146_096 } / 146_097;
    let doe = l - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fingerprint_is_stable_and_length_prefixed() {
        let a = fingerprint(&[b"hello", b"world"]);
        let b = fingerprint(&[b"hello", b"world"]);
        let c = fingerprint(&[b"helloworld"]);
        let d = fingerprint(&[b"hell", b"oworld"]);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        // Concatenations must not collide ("hello"+"world" != "helloworld").
        assert_ne!(a, c);
        assert_ne!(c, d);
    }

    #[test]
    fn usage_merges_all_three_api_shapes() {
        // Anthropic message_start / message_delta.
        let mut u = Usage::default();
        u.merge_json(&json!({
            "type": "message_start",
            "message": {"usage": {"input_tokens": 100, "cache_read_input_tokens": 40, "cache_creation_input_tokens": 5}}
        }));
        u.merge_json(&json!({"type": "message_delta", "usage": {"output_tokens": 9}}));
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.cache_read, Some(40));
        assert_eq!(u.cache_creation, Some(5));
        assert_eq!(u.output_tokens, Some(9));

        // Chat Completions final object.
        let mut u = Usage::default();
        u.merge_json(&json!({"usage": {
            "prompt_tokens": 55, "completion_tokens": 7,
            "prompt_tokens_details": {"cached_tokens": 12}
        }}));
        assert_eq!(u.input_tokens, Some(55));
        assert_eq!(u.output_tokens, Some(7));
        assert_eq!(u.cache_read, Some(12));

        // Responses API response.completed.
        let u = usage_from_json(&json!({
            "response": {"usage": {"input_tokens": 80, "output_tokens": 3}}
        }));
        let u = u.unwrap();
        assert_eq!(u.input_tokens, Some(80));
        assert_eq!(u.output_tokens, Some(3));

        // Nothing usable -> None.
        assert!(usage_from_json(&json!({"unrelated": 1})).is_none());
    }

    #[test]
    fn journal_totals_split_today_and_all() {
        let dir = std::env::temp_dir().join("shrinkgate-test-totals");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let day_start = now_ms() / 86_400_000 * 86_400_000;
        let mk = |ts: u64, saved: u64| json!({"ts": ts, "saved_tokens": saved, "status": 200});
        let mut text = String::new();
        text.push_str(&format!("{}\n", mk(day_start - 10_000, 100))); // yesterday
        text.push_str(&format!("{}\n", mk(day_start + 10_000, 250))); // today
        text.push_str(&format!("{}\n", mk(day_start + 20_000, 50))); // today
        text.push_str("not json\n");
        std::fs::write(dir.join("usage.jsonl"), text).unwrap();
        let (today_s, today_n, all_s, all_n) = totals(&dir);
        assert_eq!(today_s, 300);
        assert_eq!(today_n, 2);
        assert_eq!(all_s, 400);
        assert_eq!(all_n, 3, "the unparsable line is skipped, not counted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_ymd_matches_known_dates() {
        assert_eq!(utc_ymd(0), (1970, 1, 1));
        assert_eq!(utc_ymd(946_684_800_000), (2000, 1, 1)); // 2000-01-01
        assert_eq!(utc_ymd(1_709_164_800_000), (2024, 2, 29)); // leap day
        assert_eq!(utc_ymd(1_798_761_600_000), (2027, 1, 1)); // 1.8e9 s = 2027-01-15 minus 14 days
        assert_eq!(utc_ymd(1_799_999_999_999), (2027, 1, 15)); // known: 1.8e9 s epoch
    }
    #[test]
    fn failed_attempts_are_reported_but_not_credited() {
        let dir = tempfile::tempdir().unwrap();
        let good = json!({"ts":now_ms(),"status":200,"saved_tokens":12});
        let failed = json!({"ts":now_ms(),"status":500,"saved_tokens":9999});
        let cancelled = json!({"ts":now_ms(),"status":499,"saved_tokens":9999});
        std::fs::write(
            dir.path().join("usage.jsonl"),
            format!("{good}\n{failed}\n{cancelled}\n"),
        )
        .unwrap();
        let report = summary(dir.path());
        assert_eq!(report["requests"], 3);
        assert_eq!(report["failed_or_cancelled_requests"], 2);
        assert_eq!(report["estimated_saved_tokens"], 12);
    }
}

#[cfg(test)]
mod audit_regressions {
    use super::*;
    fn record() -> Record {
        Record {
            route: "test",
            session: "fixture".into(),
            est_orig_bytes: 0,
            est_sent_bytes: 0,
            saved_bytes: 0,
            blocks_stubbed: 0,
            status: 200,
            ms: 0,
            usage: None,
            stream_outcome: None,
        }
    }
    #[tokio::test]
    async fn slow_writer_has_bounded_backlog_and_explicit_overflow() {
        let (started, ready) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let processed = Arc::new(AtomicU64::new(0));
        let count = processed.clone();
        let journal = Journal::with_writer(1, move |_| {
            if count.fetch_add(1, Ordering::Relaxed) == 0 {
                started.send(()).unwrap();
                blocked.recv().unwrap();
            }
        })
        .unwrap();
        journal.submit(record());
        ready.recv().unwrap();
        journal.submit(record());
        journal.submit(record());
        assert_eq!(journal.dropped(), 1);
        assert_eq!(journal.tx.capacity(), 0);
        release.send(()).unwrap();
        journal.flush().await;
        assert_eq!(processed.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn failed_usage_is_in_totals_without_earning_savings_credit() {
        let dir = tempfile::tempdir().unwrap();
        let failed = json!({"ts":now_ms(),"status":499,"saved_tokens":999,
            "usage":{"input_tokens":123,"output_tokens":7,"cache_read":10,"cache_creation":2}});
        std::fs::write(dir.path().join("usage.jsonl"), format!("{failed}\n")).unwrap();
        let report = summary(dir.path());
        assert_eq!(report["estimated_saved_tokens"], 0);
        assert_eq!(report["provider_usage"]["input_tokens"], 123);
        assert_eq!(report["provider_usage"]["output_tokens"], 7);
        assert_eq!(
            report["provider_usage"]["failed_or_cancelled"]["cache_read_tokens"],
            10
        );
        assert_eq!(
            report["provider_usage"]["failed_or_cancelled"]["requests_with_usage"],
            1
        );
    }
}
