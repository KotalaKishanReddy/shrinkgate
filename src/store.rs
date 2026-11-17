//! Per-session shrink state and the local usage journal.

use ring::digest::{Context, SHA256};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Stable content fingerprint (hex, 16 chars). Length-prefixed parts avoid
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
    hex16(digest.as_ref())
}

/// Lowercase hex of the first 8 bytes (16 chars).
fn hex16(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(16);
    for byte in b.iter().take(8) {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0xf) as usize] as char);
    }
    s
}

/// Crude token estimate (≈ chars/4 for code and prose).
pub fn est_tokens(bytes: usize) -> usize {
    bytes / 4
}

#[derive(Debug, Clone, Copy)]
pub struct Seen {
    /// Request ordinal within the session when this content was first sent.
    pub turn: u32,
}

#[derive(Default)]
pub struct Session {
    pub seen: HashMap<String, Seen>,
    pub turn: u32,
}

struct Inner {
    map: HashMap<String, Session>,
    lru: VecDeque<String>,
}

pub struct Sessions {
    inner: Mutex<Inner>,
    max: usize,
}

impl Sessions {
    pub fn new(max: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                lru: VecDeque::new(),
            }),
            max: max.max(8),
        }
    }

    /// Runs `f` with the session's mutable state, touching LRU order and
    /// evicting the oldest sessions beyond the cap. `f` must not await.
    pub fn with_session<R>(&self, key: &str, f: impl FnOnce(&mut Session) -> R) -> R {
        let mut g: MutexGuard<Inner> = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => PoisonError::into_inner(poisoned),
        };
        if let Some(pos) = g.lru.iter().position(|k| k == key) {
            g.lru.remove(pos);
        }
        g.lru.push_back(key.to_string());
        while g.map.len() > self.max {
            match g.lru.pop_front() {
                Some(old) if old != key => {
                    g.map.remove(&old);
                }
                Some(_) => continue,
                None => break,
            }
        }
        let session = g.map.entry(key.to_string()).or_default();
        f(session)
    }
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

pub fn journal(state_dir: &Path, rec: &Record) {
    let file = crate::paths::usage_file(state_dir);
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        let _ = writeln!(f, "{}", rec.to_json());
    }
}

/// (today_saved_tokens, today_requests, all_saved_tokens, all_requests) in UTC days.
pub fn totals(state_dir: &Path) -> (u64, u64, u64, u64) {
    let file = crate::paths::usage_file(state_dir);
    let Ok(text) = std::fs::read_to_string(&file) else {
        return (0, 0, 0, 0);
    };
    let day_start = now_ms() / 86_400_000 * 86_400_000;
    let mut today_s = 0u64;
    let mut today_n = 0u64;
    let mut all_s = 0u64;
    let mut all_n = 0u64;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let saved = v.get("saved_tokens").and_then(Value::as_u64).unwrap_or(0);
        let ts = v.get("ts").and_then(Value::as_u64).unwrap_or(0);
        all_s += saved;
        all_n += 1;
        if ts >= day_start {
            today_s += saved;
            today_n += 1;
        }
    }
    (today_s, today_n, all_s, all_n)
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
        assert_eq!(a.len(), 16);
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
    fn session_lru_evicts_oldest() {
        let s = Sessions::new(2);
        s.with_session("a", |_| {});
        s.with_session("b", |_| {});
        s.with_session("a", |sess| sess.turn += 1); // touch a: b is now oldest
        s.with_session("c", |_| {}); // evicts b
        s.with_session("b", |sess| {
            assert_eq!(sess.turn, 0, "b was evicted; its state must be fresh")
        });
        s.with_session("a", |sess| assert_eq!(sess.turn, 1, "a survived"));
    }
}
