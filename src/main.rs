//! opencode-sub — relays OpenAI Responses turns to OpenCode Zen/Go
//! (opencode.ai/zen[/go]/v1) with the user's API key. No subprocess.
//! The free-tier `opencode serve` relay was removed: it fingerprinted the
//! free models to the OpenCode client itself, and relaying them elsewhere
//! runs against Anomaly's terms. Zen + Go API-key providers only.

use acp_bridge::relay::{self, Intents, TurnFn};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

mod setup;
mod usage;
mod zen;

use setup::ZenKind;

const PROVIDER_ZEN: &str = "opencode-zen";
const PROVIDER_GO: &str = "opencode-go";
const AUTH_ZEN: &str = "zen-api-key";
const AUTH_GO: &str = "go-api-key";
const TOKEN_PREFIX: &str = "opencode-sub-";

fn manifest() -> Value {
    let provider = |id: &str, name: &str, auth: &str, label: &str| {
        json!({
            "id": id,
            "name": name,
            "auth_methods": [{
                "id": auth, "kind": "api_key", "name": label,
                "operations": ["models", "chat", "refresh"],
            }],
            "transport": {
                "kind": "openai-responses",
                "base_url": "https://127.0.0.1:1/",
                "authorization": {"kind": "bearer", "secret_name": "relay_token"},
            },
        })
    };
    json!({
        "name": "opencode-sub",
        "display_name": "OpenCode",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.2",
        "capabilities": ["provider.credentials", "host.say"],
        "providers": [
            provider(PROVIDER_ZEN, "OpenCode Zen", AUTH_ZEN, "Zen API key"),
            provider(PROVIDER_GO, "OpenCode Go", AUTH_GO, "Go API key"),
        ],
        "commands": ["/opencode"],
    })
}

#[derive(Debug)]
enum RpcError {
    Protocol(String),
    Unavailable(String),
}

fn unavailable(msg: impl Into<String>) -> RpcError {
    RpcError::Unavailable(msg.into())
}

fn error_value(e: &RpcError) -> Value {
    match e {
        RpcError::Unavailable(message) => json!({
            "code": "unavailable", "message": message, "retryable": true, "terminal": false,
        }),
        RpcError::Protocol(message) => json!({
            "code": "protocol", "message": message, "retryable": false, "terminal": true,
        }),
    }
}

fn kind_for(provider: &str) -> Option<ZenKind> {
    match provider {
        PROVIDER_ZEN => Some(ZenKind::Zen),
        PROVIDER_GO => Some(ZenKind::Go),
        _ => None,
    }
}

fn kind_for_method(method: &str) -> Option<ZenKind> {
    match method {
        AUTH_ZEN => Some(ZenKind::Zen),
        AUTH_GO => Some(ZenKind::Go),
        _ => None,
    }
}

fn auth_refresh(params: &Value) -> Result<Value, RpcError> {
    let method = params.get("auth_method").and_then(Value::as_str).unwrap_or_default();
    let Some(kind) = kind_for_method(method) else {
        return Err(RpcError::Protocol("unknown auth method".into()));
    };
    let key = setup::api_key(kind).ok_or_else(|| unavailable(setup::LOGIN_HINT))?;
    let _ = setup::write_cache(&key);
    Ok(json!({"credential": {
        "secrets": {"api_key": key}, "metadata": {}, "expires_at": null,
    }}))
}

fn auth_revoke(_params: &Value) -> Result<Value, RpcError> {
    // Drop only this plugin's cache; OpenCode's own files are never touched.
    if let Some(home) = std::env::home_dir() {
        let _ = std::fs::remove_file(home.join(".opencode-sub.json"));
    }
    Ok(json!({}))
}

fn provider_models(params: &Value) -> Result<Value, RpcError> {
    let provider = params.get("provider").and_then(Value::as_str).unwrap_or_default();
    let Some(kind) = kind_for(provider) else {
        return Err(RpcError::Protocol("unknown provider".into()));
    };
    if let Some(key) = params
        .get("credential")
        .and_then(|c| c.get("material"))
        .and_then(|m| m.get("secrets"))
        .and_then(|s| s.get("api_key"))
        .and_then(Value::as_str)
    {
        let _ = setup::write_cache(key);
    }
    let key = setup::api_key(kind).ok_or_else(|| unavailable(setup::LOGIN_HINT))?;
    let rows = zen::list_models(kind, &key).map_err(unavailable)?;
    Ok(json!({
        "provider": provider,
        "models": rows,
        "default_model": rows.first().and_then(|m| m.get("id")).cloned().unwrap_or(Value::Null),
    }))
}

fn provider_chat(intents: &Intents, params: &Value) -> Result<Value, RpcError> {
    let provider = params.get("provider").and_then(Value::as_str).unwrap_or_default();
    let Some(kind) = kind_for(provider) else {
        return Err(RpcError::Protocol("unknown provider".into()));
    };
    let key = setup::api_key(kind).ok_or_else(|| unavailable(setup::LOGIN_HINT))?;
    let model = params.get("model").and_then(Value::as_str).unwrap_or_default();
    let key2 = key.clone();
    let turn: TurnFn = Arc::new(move |raw: &[u8], cancel: &AtomicBool| {
        zen::forward(kind, &key2, raw, cancel)
    });
    let (relay_url, relay_token) =
        relay::mint_turn(intents, model, turn, TOKEN_PREFIX).map_err(unavailable)?;
    Ok(json!({"relay_url": relay_url, "relay_token": relay_token}))
}

fn command_run(params: &Value) -> Result<Value, RpcError> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    if name != "/opencode" {
        return Err(RpcError::Protocol(format!("unknown command {name}")));
    }
    let argv: Vec<String> = params
        .get("argv")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    match argv.first().map(String::as_str).unwrap_or("") {
        "" => Ok(json!({})),
        "status" => {
            let zen = setup::api_key(ZenKind::Zen).is_some();
            let go = setup::api_key(ZenKind::Go).is_some();
            let key_txt = match (zen, go) {
                (true, true) => "keys configured (zen + go)",
                (true, false) | (false, true) => "key configured",
                (false, false) => setup::LOGIN_HINT,
            };
            Ok(json!({"text": format!("OpenCode: {key_txt}")}))
        }
        _ => Err(RpcError::Protocol("usage: /opencode [status]".into())),
    }
}

fn dispatch(method: &str, params: &Value, intents: &Intents) -> Result<Value, RpcError> {
    match method {
        "plugin/manifest" => Ok(manifest()),
        "provider/auth/start" => Err(RpcError::Protocol(
            "api-key providers refresh instead of device login".into(),
        )),
        "provider/auth/poll" | "provider/auth/cancel" => Ok(json!({"state": "failed", "error": {
            "code": "protocol", "message": "no login operation",
            "retryable": false, "terminal": true,
        }})),
        "provider/auth/refresh" => auth_refresh(params),
        "provider/auth/revoke" => auth_revoke(params),
        "provider/usage" => usage::handle(&params).map_err(unavailable),
        "provider/models" => provider_models(params),
        "provider/chat" => provider_chat(intents, params),
        "command/run" => command_run(params),
        "plugin/tools" => Ok(json!({"tools": []})),
        "event/notify" | "plugin/shutdown" => Ok(json!({})),
        _ => Err(RpcError::Protocol(format!("unknown method {method}"))),
    }
}

fn write_response(out: &mut impl Write, id: Value, outcome: Result<Value, RpcError>) -> bool {
    let response = match outcome {
        Ok(result) => json!({"id": id, "result": result}),
        Err(e) => json!({"id": id, "error": error_value(&e)}),
    };
    writeln!(out, "{response}")
        .and_then(|()| out.flush())
        .is_ok()
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest());
        return;
    }
    let intents: Intents = Default::default();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = dispatch(&method, &params, &intents);
        if !id.is_null() && !write_response(&mut out, id, result) {
            return;
        }
        if method == "plugin/shutdown" {
            break;
        }
    }
}
