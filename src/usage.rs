//! `provider/usage`: OpenCode (zen/go) subscription card.
//!
//! The zen/go endpoints are API-key passthroughs — quota and billing
//! live on the opencode.ai dashboard, not in a fetchable API. The card
//! reports which flavor is connected with no fabricated windows.

use serde_json::{Value, json};

use crate::setup::{self, ZenKind};

/// `provider/usage` entry: the auth method selects the zen flavor.
pub fn handle(params: &Value) -> Result<Value, String> {
    let method = params
        .get("auth_method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = crate::kind_for_method(method)
        .ok_or_else(|| format!("unknown auth method {method}"))?;
    if setup::api_key(kind).is_none() {
        return Err("no OpenCode key".to_string());
    }
    let title = match kind {
        ZenKind::Zen => "OpenCode Zen",
        ZenKind::Go => "OpenCode Go",
    };
    Ok(json!({
        "available": true,
        "title": title,
        "windows": [],
        "note": "billing lives on opencode.ai — the API exposes no usage counter",
    }))
}
