//! Claude Code statusline: one line with today's local savings.

use std::io::Read;

pub fn run() -> String {
    // Drain Claude Code's status JSON from stdin; we don't need it.
    let mut buf = Vec::new();
    let _ = std::io::stdin().take(1 << 20).read_to_end(&mut buf);
    let (today_saved, today_reqs, all_saved, all_reqs) =
        crate::store::totals(&crate::paths::state_dir());
    let (y, m, d) = crate::store::utc_ymd(crate::store::now_ms());
    format!(
        "ShrinkGate {y:04}-{m:02}-{d:02}: −{today_saved} est. tok today (UTC) ({today_reqs} req) · −{all_saved} est. tok retained ({all_reqs} req)"
    )
}
