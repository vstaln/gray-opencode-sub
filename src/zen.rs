//! The turn forwarder: POST the raw Responses body to Zen with the bearer
//! key; stream the SSE body back verbatim.

use crate::setup::ZenKind;
use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_BODY: usize = 16 * 1024 * 1024;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(UPSTREAM_TIMEOUT)
        .build()
}

/// The relay TurnFn for one zen flavor.
pub fn forward(kind: ZenKind, key: &str, raw: &[u8], _cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    // Model id may carry bracket overrides or provider prefixes zen
    // doesn't know; pass the body through untouched otherwise.
    let url = format!("{}/responses", kind.base_url());
    let resp = agent()
        .post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .set("Accept", "text/event-stream")
        .send_bytes(raw);
    match resp {
        Ok(r) => {
            let mut buf = Vec::new();
            r.into_reader()
                .take(MAX_BODY as u64)
                .read_to_end(&mut buf)
                .map_err(|e| format!("zen stream read: {e}"))?;
            if buf.is_empty() {
                return Err("zen returned an empty response".into());
            }
            Ok(buf)
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let body: String = body.chars().take(600).collect();
            Err(format!("zen HTTP {code}: {}", redact_key(&body, key)))
        }
        Err(e) => Err(format!("zen request: {e}")),
    }
}

fn redact_key(body: &str, key: &str) -> String {
    if key.is_empty() {
        body.to_string()
    } else {
        body.replace(key, "[key]")
    }
}

/// GET {base}/models → catalog rows.
pub fn list_models(kind: ZenKind, key: &str) -> Result<Vec<serde_json::Value>, String> {
    let url = format!("{}/models", kind.base_url());
    let resp = agent()
        .get(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .call();
    let body = match resp {
        Ok(r) => r.into_string().map_err(|e| format!("zen models read: {e}"))?,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let body: String = body.chars().take(400).collect();
            return Err(format!("zen models HTTP {code}: {}", redact_key(&body, key)));
        }
        Err(e) => return Err(format!("zen models: {e}")),
    };
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("zen models json: {e}"))?;
    let items = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rows = items
        .iter()
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("modelId"))
                .and_then(serde_json::Value::as_str)?;
            Some(serde_json::json!({
                "id": id,
                "name": m.get("name").and_then(serde_json::Value::as_str).unwrap_or(id),
            }))
        })
        .collect();
    Ok(rows)
}
