//! Credential resolution for OpenCode Zen/Go: the user's own API key from
//! OPENCODE_API_KEY, or the gray-managed cache written from the key the host
//! collected. OpenCode's own credential store is never read.

use serde_json::Value;
use std::path::PathBuf;

pub const LOGIN_HINT: &str =
    "no OpenCode key — create one at https://opencode.ai/auth, then enter it in the provider login (or set OPENCODE_API_KEY)";

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
}

/// Resolution order: `OPENCODE_API_KEY`, then this plugin's own cache.
/// Zen and Go take the same key, so the flavor doesn't change the lookup.
pub fn api_key(_kind: ZenKind) -> Option<String> {
    if let Ok(k) = std::env::var("OPENCODE_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    read_cache()
}


fn cache_path() -> Option<PathBuf> {
    std::env::home_dir().map(|h| h.join(".opencode-sub.json"))
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
