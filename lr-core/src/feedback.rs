//! Automatic diagnostic feedback primitives shared by the desktop and PE clients.
//! The server never receives raw paths, account names or authorization material.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEnvelope {
    pub protocol: u16,
    pub client_version: String,
    pub session_id: String,
    pub stage: String,
    pub log: String,
}

pub fn sanitize_log(input: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_bytes));
    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("password") || lower.contains("recoverypassword") || lower.contains("private key") {
            out.push_str("[redacted sensitive line]\n");
            continue;
        }
        let mut line = line.replace("C:\\Users\\", "%USERPROFILE%\\");
        if let Some(pos) = line.find("Bearer ") { line.replace_range(pos.., "Bearer [redacted]"); }
        out.push_str(&line);
        out.push('\n');
        if out.len() >= max_bytes { break; }
    }
    out.truncate(max_bytes);
    out
}

pub fn envelope(log: &str, client_version: &str, session_id: &str, stage: &str) -> FeedbackEnvelope {
    FeedbackEnvelope { protocol: 1, client_version: client_version.into(), session_id: session_id.into(), stage: stage.into(), log: sanitize_log(log, 2 * 1024 * 1024) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_sensitive_material_and_bounds_output() {
        let s = sanitize_log("password=secret\nC:\\Users\\alice\\x\n", 128);
        assert!(!s.contains("secret"));
        assert!(s.contains("%USERPROFILE%"));
    }
}
