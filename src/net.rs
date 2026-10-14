//! Tiny HTTP layer: curl (everywhere) and `gh api` (authenticated, higher GitHub rate limit) when present.
use crate::model::has_cmd;
use serde_json::Value;
use std::process::Command;

pub fn get(url: &str) -> Option<String> {
    let o = Command::new("curl").args(["-fsSL", "-m", "10", url]).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn json(url: &str) -> Option<Value> {
    serde_json::from_str(&get(url)?).ok()
}

/// A file from a GitHub repo's default branch, parsed as JSON.
pub fn raw_json(repo: &str, path: &str) -> Option<Value> {
    let path = path.trim_start_matches("./").trim_matches('/');
    json(&format!("https://raw.githubusercontent.com/{repo}/HEAD/{path}"))
}

/// GitHub REST API (`repos/o/r/...`).
pub fn github(path: &str) -> Option<Value> {
    if has_cmd("gh") {
        if let Ok(o) = Command::new("gh").args(["api", path]).output() {
            if o.status.success() {
                return serde_json::from_slice(&o.stdout).ok();
            }
        }
    }
    json(&format!("https://api.github.com/{path}"))
}
