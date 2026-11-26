use crate::{
    relay::Stream,
    server::{Runtime, Server},
};
use gray_acp::chat::{
    assistant_side, item_to_transcript_pub, now_secs, prepare_turn, PreparedTurn,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_SESSIONS: usize = 128;
const HEARTBEAT: Duration = Duration::from_secs(12);
const START_TIMEOUT: Duration = Duration::from_secs(45);
const TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DISCONNECTED: &str = "The Gray relay disconnected";

type SessionLock = Arc<Mutex<Option<Session>>>;

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<(String, String), SessionLock>>);

struct Session {
    generation: Uuid,
    id: String,
    model: Value,
    instructions: String,
    absorbed: (usize, u64),
    reply: u64,
    touched: Instant,
}

impl Sessions {
    fn entry(&self, provider: &str, key: Option<&str>) -> Result<SessionLock, String> {
        let Some(key) = key.filter(|k| !k.is_empty()) else {
            return Ok(Default::default());
        };
        let mut map = self.0.lock().map_err(|_| "OpenCode sessions lock failed")?;
        let entry = (provider.to_string(), key.to_string());
        if let Some(state) = map.get(&entry) {
            return Ok(Arc::clone(state));
        }
        if map.len() >= MAX_SESSIONS {
            let oldest = map
                .iter()
                .filter_map(|(key, state)| {
                    if Arc::strong_count(state) != 1 {
                        return None;
                    }
                    let state = state.try_lock().ok()?;
                    Some((key, state.as_ref().map(|s| s.touched)))
                })
                .min_by_key(|(_, time)| *time)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                map.remove(&oldest);
            } else {
                return Err("Too many active OpenCode sessions".into());
            }
        }
        Ok(Arc::clone(map.entry(entry).or_default()))
    }
}

fn digest(items: &[Value]) -> u64 {
    let mut hash = DefaultHasher::new();
    for item in items {
        item.to_string().hash(&mut hash);
    }
    hash.finish()
}

fn digest_text(text: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    text.trim().hash(&mut hash);
    hash.finish()
}

fn echo_text(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(item_to_transcript_pub)
        .filter_map(|s| s.strip_prefix("Assistant: ").map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn continuation(session: &Session, turn: &PreparedTurn, server: &Server) -> Option<usize> {
    let (seen, prefix) = session.absorbed;
    if session.generation != server.generation
        || seen >= turn.input.len()
        || digest(&turn.input[..seen]) != prefix
    {
        return None;
    }
    let rest = &turn.input[seen..];
    let echo = rest.iter().take_while(|item| assistant_side(item)).count();
    if echo == rest.len()
        || (echo == 0 && session.reply != digest_text(""))
        || (echo > 0
            && (rest[..echo]
                .iter()
                .any(|item| item["type"] == "function_call")
                || digest_text(&echo_text(&rest[..echo])) != session.reply))
    {
        return None;
    }
    Some(seen + echo)
}

fn model_ref(rows: &[Value], provider: &str, turn: &PreparedTurn) -> Result<Value, String> {
    let name = turn
        .model
        .strip_prefix(&format!("{provider}/"))
        .or_else(|| {
            (provider == "opencode")
                .then(|| turn.model.strip_prefix("opencode-zen/"))
                .flatten()
        })
        .unwrap_or(&turn.model);
    let (id, suffix) = name
        .split_once('#')
        .map_or((name, None), |(id, variant)| (id, Some(variant)));
    let row = if id.is_empty() {
        rows.first()
    } else {
        rows.iter().find(|m| m["id"] == id)
    }
    .ok_or_else(|| format!("Model {id:?} is not available from OpenCode provider {provider}"))?;
    let valid = |id: &str| {
        id == "default"
            || row["variants"]
                .as_array()
                .is_some_and(|vs| vs.iter().any(|v| v["id"] == id))
    };
    let explicit = suffix.or_else(|| {
        turn.overrides
            .iter()
            .find(|(k, _)| k == "variant" || k == "effort")
            .map(|(_, v)| v.as_str())
    });
    if let Some(variant) = explicit.filter(|id| !valid(id)) {
        return Err(format!(
            "OpenCode model {} has no variant {variant:?}",
            row["id"]
        ));
    }
    let variant = explicit
        .or_else(|| turn.effort.as_deref().filter(|id| valid(id)))
        .unwrap_or("default");
    Ok(json!({"providerID": provider, "id": row["id"], "variant": variant}))
}

fn path_id(value: &Value) -> Option<&str> {
    value.as_str().filter(|id| {
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn prompt(items: &[Value], id: &str) -> Value {
    let mut text = Vec::new();
    let mut files = Vec::new();
    for item in items {
        if matches!(item["role"].as_str(), Some("system" | "developer")) {
            continue;
        }
        if let Some(line) = item_to_transcript_pub(item) {
            text.push(line);
        }
        if let Some(parts) = item["content"].as_array() {
            for part in parts {
                let (uri, name) = match part["type"].as_str() {
                    Some("input_image") => (part["image_url"].as_str(), None),
                    Some("input_file") => (part["file_data"].as_str(), part["filename"].as_str()),
                    _ => continue,
                };
                if let Some(uri) = uri.filter(|uri| uri.starts_with("data:")) {
                    let mut file = json!({"uri": uri});
                    if let Some(name) = name {
                        file["name"] = json!(name);
                    }
                    files.push(file);
                }
            }
        }
    }
    let mut body = json!({"id": id, "text": text.join("\n\n")});
    if !files.is_empty() {
        body["files"] = json!(files);
    }
    body
}

struct Execution<'a> {
    server: &'a Server,
    session: &'a str,
    message: &'a str,
    settled: bool,
}

impl Drop for Execution<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let base = format!("/api/session/{}", self.session);
            let _ = self
                .server
                .json("DELETE", &format!("{base}/inbox/{}", self.message), None);
            let _ = self.server.json("POST", &format!("{base}/interrupt"), None);
        }
    }
}

pub struct Turn {
    pub runtime: Arc<Runtime>,
    pub sessions: Arc<Sessions>,
    pub provider: &'static str,
    pub key: String,
    pub model: String,
}

impl Turn {
    pub fn run(self, raw: &[u8], isolated: bool, stream: &mut Stream<'_>) -> Result<(), String> {
        let Self {
            runtime,
            sessions,
            provider,
            key,
            model,
        } = self;
        let body: Value =
            serde_json::from_slice(raw).map_err(|_| "Invalid Gray Responses request")?;
        let turn = prepare_turn(&body, &model)?;
        let session_key = (!isolated)
            .then(|| body["prompt_cache_key"].as_str())
            .flatten();
        let entry = sessions.entry(provider, session_key)?;
        let mut state = entry
            .try_lock()
            .map_err(|_| "This Gray conversation already has an OpenCode turn in progress")?;
        let server = runtime.connect(&key)?;
        let chosen = model_ref(&server.models(provider)?, provider, &turn)?;
        let prior = state.take();
        let start = prior.as_ref().and_then(|s| continuation(s, &turn, &server));
        let mut session = if let (Some(prior), Some(_)) = (prior, start) {
            prior
        } else {
            let data = server.json("POST", "/api/session", Some(json!({
            "location": {"directory": server.directory}, "title": "Gray",
            "model": chosen, "permissions": [{"action": "*", "resource": "*", "effect": "deny"}],
        })))?;
            let id = path_id(&data["data"]["id"]).ok_or("OpenCode did not return a session ID")?;
            Session {
                generation: server.generation,
                id: id.to_string(),
                model: chosen.clone(),
                instructions: String::new(),
                absorbed: (0, 0),
                reply: 0,
                touched: Instant::now(),
            }
        };
        let base = format!("/api/session/{}", session.id);
        if chosen != session.model {
            server.json(
                "POST",
                &format!("{base}/model"),
                Some(json!({"model": chosen})),
            )?;
            session.model = chosen;
        }
        if turn.system != session.instructions {
            let path = format!(
                "/api/experimental/session/{}/instructions/entries/gray",
                session.id
            );
            if turn.system.is_empty() {
                server.json("DELETE", &path, None)?;
            } else {
                server.json("PUT", &path, Some(json!({"value": turn.system})))?;
            }
            session.instructions = turn.system;
        }
        let events = server.events()?;
        if !stream.connected() {
            return Err(DISCONNECTED.into());
        }
        let message = format!("msg_{}", Uuid::new_v4().simple());
        let mut execution = Execution {
            server: &server,
            session: &session.id,
            message: &message,
            settled: false,
        };
        server.json(
            "POST",
            &format!("{base}/prompt"),
            Some(prompt(&turn.input[start.unwrap_or(0)..], &message)),
        )?;
        let mut responses = Responses::new(&model, stream)?;
        let mut started = false;
        let began = Instant::now();
        let mut heartbeat = began;
        loop {
            if !responses.stream.connected() {
                return Err(DISCONNECTED.into());
            }
            if heartbeat.elapsed() >= HEARTBEAT {
                responses.stream.heartbeat().map_err(|_| DISCONNECTED)?;
                heartbeat = Instant::now();
            }
            if (!started && began.elapsed() > START_TIMEOUT) || began.elapsed() > TURN_TIMEOUT {
                return Err("Timed out waiting for OpenCode execution".into());
            }
            let event = match events.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => event?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("The OpenCode event stream closed before completion".into())
                }
            };
            let data = &event["data"];
            if data["sessionID"] != session.id && data["form"]["sessionID"] != session.id {
                continue;
            }
            match event["type"].as_str().unwrap_or_default() {
                "session.execution.started" => started = true,
                "session.inbox.cancelled" if data["inboxID"] == message => {
                    return Err("OpenCode cancelled the prompt before execution".into())
                }
                _ if !started => continue,
                "session.execution.succeeded" => {
                    execution.settled = true;
                    break;
                }
                "session.execution.interrupted" => {
                    execution.settled = true;
                    return Err("OpenCode interrupted the turn".into());
                }
                "session.execution.failed" => {
                    execution.settled = true;
                    let detail = data
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("OpenCode execution failed");
                    return Err(server.redact(&detail.chars().take(800).collect::<String>()));
                }
                "permission.asked" => {
                    if let Some(id) = path_id(&data["id"]) {
                        server.json(
                            "POST",
                            &format!("{base}/permission/{id}/reply"),
                            Some(json!({"decision": "reject"})),
                        )?;
                    }
                }
                "form.created" => {
                    if let Some(id) = path_id(&data["form"]["id"]) {
                        server.json("DELETE", &format!("{base}/form/{id}"), None)?;
                    }
                }
                typ if typ.starts_with("session.text.")
                    || typ.starts_with("session.reasoning.") =>
                {
                    responses.text(typ, data)?
                }
                "session.step.ended" | "session.step.failed" => {
                    responses.record_usage(&data["tokens"])
                }
                _ => {}
            }
        }
        drop(execution);
        responses.complete()?;
        session.reply = digest_text(&responses.reply());
        session.absorbed = (turn.input.len(), digest(&turn.input));
        session.touched = Instant::now();
        *state = Some(session);
        Ok(())
    }
}

struct Part {
    item_id: String,
    reasoning: bool,
    text: String,
    done: bool,
}

impl Part {
    fn item(&self, done: bool) -> Value {
        let content = if done { self.text.as_str() } else { "" };
        if self.reasoning {
            json!({"id": self.item_id, "type": "reasoning", "summary": if done { json!([{"type": "summary_text", "text": content}]) } else { json!([]) }})
        } else {
            json!({"id": self.item_id, "type": "message", "role": "assistant", "status": if done { "completed" } else { "in_progress" },
                "content": [{"type": "output_text", "text": content, "annotations": []}]})
        }
    }
}

struct Responses<'a, 'b> {
    stream: &'a mut Stream<'b>,
    id: String,
    model: String,
    created: u64,
    usage: [u64; 4],
    parts: Vec<Part>,
    indexes: HashMap<(String, u64, bool), usize>,
}

impl<'a, 'b> Responses<'a, 'b> {
    fn new(model: &str, stream: &'a mut Stream<'b>) -> Result<Self, String> {
        let mut res = Self {
            stream,
            id: format!("resp_{}", Uuid::new_v4().simple()),
            model: model.to_string(),
            created: now_secs(),
            usage: [0; 4],
            parts: Vec::new(),
            indexes: HashMap::new(),
        };
        let response = res.response(false);
        res.emit(json!({"type": "response.created", "response": response}))?;
        Ok(res)
    }

    fn emit(&mut self, event: Value) -> Result<(), String> {
        self.stream.event(&event).map_err(|_| DISCONNECTED.into())
    }

    fn text(&mut self, typ: &str, data: &Value) -> Result<(), String> {
        let reasoning = typ.starts_with("session.reasoning.");
        let key = (
            data["assistantMessageID"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            data["ordinal"].as_u64().unwrap_or_default(),
            reasoning,
        );
        let index = if let Some(&idx) = self.indexes.get(&key) {
            idx
        } else {
            let idx = self.parts.len();
            self.indexes.insert(key, idx);
            let item_id = format!("{}_{idx}", if reasoning { "rs" } else { "msg" });
            let part = Part {
                item_id,
                reasoning,
                text: String::new(),
                done: false,
            };
            self.emit(json!({"type": "response.output_item.added", "output_index": idx, "item": part.item(false)}))?;
            self.parts.push(part);
            idx
        };
        let part = &mut self.parts[index];
        if part.done {
            return Ok(());
        }
        let delta = if typ.ends_with(".delta") {
            data["delta"].as_str()
        } else if typ.ends_with(".ended") {
            data["text"]
                .as_str()
                .and_then(|text| text.strip_prefix(part.text.as_str()))
        } else {
            None
        };
        if let Some(delta) = delta.filter(|s| !s.is_empty()) {
            part.text.push_str(delta);
            let event = json!({"type": if reasoning { "response.reasoning_summary_text.delta" } else { "response.output_text.delta" },
                "item_id": part.item_id, "output_index": index, "content_index": 0, "summary_index": 0, "delta": delta});
            self.emit(event)?;
        }
        if typ.ends_with(".ended") {
            self.end_part(index)?;
        }
        Ok(())
    }

    fn end_part(&mut self, index: usize) -> Result<(), String> {
        let part = &mut self.parts[index];
        if part.done {
            return Ok(());
        }
        part.done = true;
        let event = json!({"type": "response.output_item.done", "output_index": index, "item": part.item(true)});
        self.emit(event)
    }

    fn record_usage(&mut self, tokens: &Value) {
        let read = |path| {
            tokens
                .pointer(path)
                .and_then(Value::as_u64)
                .unwrap_or_default()
        };
        let cache = read("/cache/read");
        let reasoning = read("/reasoning");
        self.usage[0] += read("/input") + cache + read("/cache/write");
        self.usage[1] += read("/output") + reasoning;
        self.usage[2] += cache;
        self.usage[3] += reasoning;
    }

    fn response(&self, done: bool) -> Value {
        let mut response = json!({"id": self.id, "object": "response", "created_at": self.created, "model": self.model,
            "status": if done { "completed" } else { "in_progress" }, "output": []});
        if done {
            let [input, output, cached, reasoning] = self.usage;
            response["output"] = Value::Array(self.parts.iter().map(|p| p.item(true)).collect());
            response["usage"] = json!({"input_tokens": input, "output_tokens": output, "total_tokens": input + output,
                "input_tokens_details": {"cached_tokens": cached}, "output_tokens_details": {"reasoning_tokens": reasoning}});
        }
        response
    }

    fn complete(&mut self) -> Result<(), String> {
        for index in 0..self.parts.len() {
            self.end_part(index)?;
        }
        self.emit(json!({"type": "response.completed", "response": self.response(true)}))
    }

    fn reply(&self) -> String {
        self.parts
            .iter()
            .filter(|p| !p.reasoning && !p.text.is_empty())
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}
