//! Credential resolution for OpenCode Zen/Go: OPENCODE_API_KEY env,
//! opencode's own auth.json (api-key entries for `opencode`/`opencode-go`),
//! or the gray-managed cache written from host-collected material.

use serde_json::Value;
use std::path::PathBuf;

pub const LOGIN_HINT: &str =
    "no OpenCode key — get one at https://opencode.ai/auth (or `opencode auth login`)";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ZenKind {
    Zen,
    Go,
}

impl ZenKind {
    pub fn base_url(&self) -> &'static str {
        match self {
            Self::Zen => "https://opencode.ai/zen/v1",
            Self::Go => "https://opencode.ai/zen/go/v1",
        }
    }
    /// auth.json entry name for this flavor.
    pub fn provider_id(&self) -> &'static str {
        match self {
            Self::Zen => "opencode",
            Self::Go => "opencode-go",
        }
    }
}

/// Resolution order: env var → auth.json (matching then either entry) → cache.
pub fn api_key(kind: ZenKind) -> Option<String> {
    if let Ok(k) = std::env::var("OPENCODE_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    auth_json_key(kind.provider_id())
        .or_else(|| auth_json_key(other_id(kind.provider_id())))
        .or_else(read_cache)
}

fn other_id(id: &str) -> &'static str {
    if id == "opencode" {
        "opencode-go"
    } else {
        "opencode"
    }
}

fn auth_json_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("OPENCODE_AUTH_JSON") {
        return Some(PathBuf::from(p));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".local/share/opencode/auth.json"))
}

/// `{ "<provider>": {"type":"api","key":"..."} }` — api entries only.
fn auth_json_key(provider_id: &str) -> Option<String> {
    let raw = std::env::var("OPENCODE_AUTH_CONTENT")
        .ok()
        .or_else(|| std::fs::read_to_string(auth_json_path()?).ok())?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let entry = v.get(provider_id)?;
    if entry.get("type").and_then(Value::as_str) != Some("api") {
        return None;
    }
    let key = entry.get("key").and_then(Value::as_str)?.trim().to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

fn cache_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".opencode-sub.json"))
}

pub fn write_cache(key: &str) -> std::io::Result<()> {
    let Some(path) = cache_path() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).truncate(true).write(true).mode(0o600);
        let mut f = opts.open(path)?;
        use std::io::Write;
        f.write_all(format!("{}\n", serde_json::json!({"api_key": key})).as_bytes())
    }
    #[cfg(not(unix))]
    std::fs::write(path, format!("{}\n", serde_json::json!({"api_key": key})))
}

pub fn read_cache() -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(cache_path()?).ok()?).ok()?;
    let k = v.get("api_key").and_then(Value::as_str)?.trim().to_string();
    if k.is_empty() {
        None
    } else {
        Some(k)
    }
}
