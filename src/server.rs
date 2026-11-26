use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(12);
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);
const SSE_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Default)]
pub struct Runtime(Mutex<RuntimeState>);

#[derive(Default)]
struct RuntimeState {
    current: Option<(Arc<Server>, Option<Process>)>,
    closed: bool,
}

struct Process {
    child: Child,
    _profile: tempfile::TempDir,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Server {
    pub generation: Uuid,
    pub directory: PathBuf,
    endpoint: String,
    authorization: String,
    http: ureq::Agent,
    key: String,
}

fn agent(timeout: Duration) -> ureq::AgentBuilder {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(timeout)
        .timeout_write(HTTP_TIMEOUT)
}

fn request_error(path: &str, err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, _) => format!("OpenCode {path} returned HTTP {code}"),
        ureq::Error::Transport(_) => format!("Could not reach OpenCode {path}"),
    }
}

fn loopback_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn launch(directory: &Path, key: &str, password: &str) -> Result<(String, Process), String> {
    let profile = tempfile::Builder::new()
        .prefix("gray-opencode-")
        .tempdir()
        .map_err(|e| format!("Could not isolate the OpenCode profile: {e}"))?;
    let mut cmd =
        Command::new(std::env::var_os("OPENCODE_BIN").unwrap_or_else(|| "opencode".into()));
    cmd.args(["serve", "--hostname", "127.0.0.1", "--port", "0"])
        .current_dir(directory)
        .env_clear()
        .env("HOME", profile.path())
        .env("USERPROFILE", profile.path())
        .env("TMPDIR", profile.path())
        .env("TMP", profile.path())
        .env("TEMP", profile.path())
        .env("APPDATA", profile.path().join("config"))
        .env("LOCALAPPDATA", profile.path().join("data"))
        .env("OPENCODE_PASSWORD", password)
        .env("OPENCODE_API_KEY", key)
        .env("OPENCODE_DISABLE_AUTOUPDATE", "true")
        .env("OPENCODE_DISABLE_DEFAULT_PLUGINS", "true")
        .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    for (name, child) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = profile.path().join(child);
        fs::create_dir_all(&path).map_err(|e| format!("Could not isolate OpenCode {name}: {e}"))?;
        cmd.env(name, path);
    }
    for name in ["PATH", "SystemRoot", "WINDIR", "LANG", "LC_ALL"] {
        if let Some(value) = std::env::var_os(name) {
            cmd.env(name, value);
        }
    }
    let mut child = cmd.spawn().map_err(|e| {
        format!("Could not start OpenCode 2 (set OPENCODE_BIN to its executable): {e}")
    })?;
    let stdout = child.stdout.take();
    let process = Process {
        child,
        _profile: profile,
    };
    let stdout = stdout.ok_or("OpenCode did not open stdout")?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("opencode-server-output".into())
        .spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            for line in lines.by_ref().map_while(Result::ok) {
                let port = line
                    .trim()
                    .strip_prefix("server listening on http://127.0.0.1:")
                    .and_then(|p| p.parse::<u16>().ok())
                    .filter(|p| *p != 0);
                if let Some(port) = port {
                    let _ = tx.send(port);
                    break;
                }
            }
            drop(tx);
            for _ in lines.map_while(Result::ok) {}
        })
        .map_err(|e| format!("Could not read OpenCode startup: {e}"))?;
    let port = rx.recv_timeout(STARTUP_TIMEOUT).map_err(|e| match e {
        mpsc::RecvTimeoutError::Timeout => "Timed out waiting for OpenCode 2 to listen",
        mpsc::RecvTimeoutError::Disconnected => {
            "OpenCode exited before listening; check that OPENCODE_BIN is OpenCode 2"
        }
    })?;
    Ok((loopback_url(port), process))
}

impl Runtime {
    pub fn connect(&self, key: &str) -> Result<Arc<Server>, String> {
        let mut state = self.0.lock().map_err(|_| "OpenCode runtime lock failed")?;
        if state.closed {
            return Err("The OpenCode sidecar is shutting down".into());
        }
        if let Some((server, process)) = state.current.as_mut() {
            if server.key == key
                && process
                    .as_mut()
                    .is_none_or(|p| matches!(p.child.try_wait(), Ok(None)))
            {
                return Ok(Arc::clone(server));
            }
        }
        state.current = None;
        let directory = match std::env::var_os("GRAY_OPENCODE_DIRECTORY") {
            Some(dir) => PathBuf::from(dir),
            None => {
                std::env::current_dir().map_err(|e| format!("Could not find the workspace: {e}"))?
            }
        }
        .canonicalize()
        .map_err(|e| format!("Could not open the OpenCode workspace: {e}"))?;
        if !directory.is_dir() || directory.to_str().is_none() {
            return Err("OpenCode requires an existing UTF-8 workspace directory".into());
        }
        let (endpoint, password, process) = match std::env::var("GRAY_OPENCODE_SERVER_URL") {
            Ok(url) if !url.trim().is_empty() => {
                let url = url.trim().trim_end_matches('/');
                let port = url
                    .strip_prefix("http://127.0.0.1:")
                    .and_then(|p| p.parse::<u16>().ok())
                    .filter(|p| *p != 0)
                    .ok_or("GRAY_OPENCODE_SERVER_URL must be http://127.0.0.1:<port>")?;
                let password = std::env::var("GRAY_OPENCODE_SERVER_PASSWORD")
                    .ok()
                    .filter(|p| !p.is_empty())
                    .ok_or("Set GRAY_OPENCODE_SERVER_PASSWORD for the supplied OpenCode server")?;
                (loopback_url(port), password, None)
            }
            _ => {
                let password = Uuid::new_v4().simple().to_string();
                let (endpoint, process) = launch(&directory, key, &password)?;
                (endpoint, password, Some(process))
            }
        };
        let server = Arc::new(Server {
            generation: Uuid::new_v4(),
            directory,
            endpoint,
            authorization: format!("Basic {}", STANDARD.encode(format!("opencode:{password}"))),
            http: agent(HTTP_TIMEOUT).timeout(HTTP_TIMEOUT).build(),
            key: key.to_string(),
        });
        let info = server.json("GET", "/api/info", None)?;
        if !info["version"]
            .as_str()
            .is_some_and(|v| v.starts_with("2."))
        {
            return Err("The server does not report an OpenCode 2 version".into());
        }
        state.current = Some((Arc::clone(&server), process));
        Ok(server)
    }

    pub fn reset(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.current = None;
        }
    }

    pub fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
            state.current = None;
        }
    }
}

impl Server {
    pub fn redact(&self, text: &str) -> String {
        text.replace(&self.key, "[redacted]")
    }

    fn request(&self, agent: &ureq::Agent, method: &str, path: &str) -> ureq::Request {
        agent
            .request(method, &format!("{}{path}", self.endpoint))
            .set("Authorization", &self.authorization)
    }

    pub fn json(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut req = self
            .request(&self.http, method, path)
            .set("Accept", "application/json");
        if path == "/api/model" {
            req = req.query("location[directory]", &self.directory.to_string_lossy());
        }
        let response = match body {
            Some(body) => req.send_json(body),
            None => req.call(),
        }
        .map_err(|e| request_error(path, e))?;
        if response.status() == 204 {
            return Ok(Value::Null);
        }
        serde_json::from_reader(response.into_reader().take(32 << 20))
            .map_err(|_| format!("OpenCode {path} returned invalid JSON"))
    }

    pub fn models(&self, provider: &str) -> Result<Vec<Value>, String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let catalog = self.json("GET", "/api/model", None)?;
            let mut rows: Vec<_> = catalog["data"]
                .as_array()
                .ok_or("OpenCode returned an invalid model catalog")?
                .iter()
                .filter(|model| {
                    model["providerID"] == provider
                        && model["enabled"] != false
                        && model["type"].as_str().is_none_or(|kind| kind == "chat")
                        && model["id"].is_string()
                        && model.pointer("/settings/apiKey").and_then(Value::as_str)
                            != Some("public")
                })
                .cloned()
                .collect();
            if !rows.is_empty() || Instant::now() >= deadline {
                rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                return Ok(rows);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn events(&self) -> Result<mpsc::Receiver<Result<Value, String>>, String> {
        let path = "/api/event";
        let response = self
            .request(&agent(SSE_TIMEOUT).build(), "GET", path)
            .set("Accept", "text/event-stream")
            .call()
            .map_err(|e| request_error(path, e))?;
        if !response
            .header("Content-Type")
            .is_some_and(|ct| ct.starts_with("text/event-stream"))
        {
            return Err("OpenCode did not return an event stream".into());
        }
        let (tx, rx) = mpsc::sync_channel(64);
        std::thread::Builder::new()
            .name("opencode-events".into())
            .spawn(move || {
                let mut reader = BufReader::new(response.into_reader());
                let mut frame = String::new();
                let mut line = String::new();
                let reason = loop {
                    line.clear();
                    match reader.by_ref().take(8 << 20).read_line(&mut line) {
                        Ok(0) => break "The OpenCode event stream closed before completion",
                        Err(_) => break "The OpenCode event stream stalled or could not be read",
                        Ok(_) => {}
                    }
                    let line = line.trim_end_matches(['\r', '\n']);
                    if line.is_empty() {
                        if let Ok(event) = serde_json::from_str(&frame) {
                            if tx.send(Ok(event)).is_err() {
                                return;
                            }
                        }
                        frame.clear();
                    } else if let Some(data) = line.strip_prefix("data:") {
                        if frame.len() + data.len() > 8 << 20 {
                            break "The OpenCode event frame was too large";
                        }
                        frame.push_str(data.strip_prefix(' ').unwrap_or(data));
                        frame.push('\n');
                    } else if line.starts_with(':') && tx.send(Ok(Value::Null)).is_err() {
                        return;
                    }
                };
                let _ = tx.send(Err(reason.into()));
            })
            .map_err(|e| format!("Could not read OpenCode events: {e}"))?;
        Ok(rx)
    }
}

pub fn public_model(model: &Value) -> Value {
    let variants: Vec<_> = model["variants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["id"].as_str())
        .filter(|v| *v != "default")
        .map(|id| json!({"id": id, "effort": id}))
        .collect();
    let efforts: Vec<_> = variants.iter().map(|v| &v["effort"]).collect();
    json!({"id": model["id"], "name": model["name"].as_str().or_else(|| model["id"].as_str()),
        "context_window": model.pointer("/limit/context").and_then(Value::as_u64),
        "reasoning_efforts": efforts, "variants": variants})
}
