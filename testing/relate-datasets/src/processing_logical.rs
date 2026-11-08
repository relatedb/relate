//! Read the cold graph's logical meter from a load-test server log.
//!
//! The child logs `logical-bytes <n>` after each successful statement when
//! `RELATE_LOG_LOGICAL_BYTES=1`. The reader waits until that log stops
//! growing so the last line is the statement that just finished.

use std::time::{Duration, Instant};

use crate::memory_controls::harness::Server;

pub(crate) fn logical_bytes(server: &Server) -> Result<u64, String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut last_len = usize::MAX;
    let mut stable_since: Option<Instant> = None;
    loop {
        let log = server.log_text();
        if log.len() == last_len {
            if stable_since.is_some_and(|since| since.elapsed() >= Duration::from_millis(50)) {
                if let Some(bytes) = latest(&log) {
                    return Ok(bytes);
                }
            } else {
                stable_since.get_or_insert_with(Instant::now);
            }
        } else {
            last_len = log.len();
            stable_since = None;
        }
        if Instant::now() >= deadline {
            return Err(format!("server never logged logical-bytes\n{log}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn latest(log: &str) -> Option<u64> {
    log.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix("logical-bytes ")
            .and_then(|rest| rest.parse().ok())
    })
}
