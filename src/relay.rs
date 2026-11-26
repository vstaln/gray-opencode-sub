use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

pub struct Stream<'a>(&'a mut TcpStream);

impl Stream<'_> {
    pub fn event(&mut self, value: &Value) -> io::Result<()> {
        write!(self.0, "data: {value}\n\n")
    }

    pub fn heartbeat(&mut self) -> io::Result<()> {
        self.0.write_all(b": keepalive\n\n")
    }

    pub fn connected(&self) -> bool {
        if self.0.set_nonblocking(true).is_err() {
            return false;
        }
        let connected = match self.0.peek(&mut [0u8; 1]) {
            Ok(n) => n != 0,
            Err(e) => e.kind() == io::ErrorKind::WouldBlock,
        };
        let _ = self.0.set_nonblocking(false);
        connected
    }
}

const ADMISSION_TTL: Duration = Duration::from_secs(180);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEADERS: u64 = 32 << 10;
const MAX_BODY: usize = 256 << 20;

type Turn = dyn FnOnce(&[u8], bool, &mut Stream<'_>) -> Result<(), String> + Send;

pub fn mint(
    turn: impl FnOnce(&[u8], bool, &mut Stream<'_>) -> Result<(), String> + Send + 'static,
) -> Result<(String, String), String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("Could not bind the OpenCode relay: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Could not read the relay address: {e}"))?
        .port();
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Could not open the OpenCode relay: {e}"))?;
    let token = format!("opencode-sub-{}", Uuid::new_v4().simple());
    let secret = token.clone();
    std::thread::Builder::new()
        .name("opencode-relay".into())
        .spawn(move || serve(listener, &secret, Box::new(turn)))
        .map_err(|e| format!("Could not start the OpenCode relay: {e}"))?;
    Ok((
        format!("http://127.0.0.1:{port}/relay/{token}/responses"),
        token,
    ))
}

fn serve(listener: TcpListener, token: &str, turn: Box<Turn>) {
    let mut end = Instant::now() + ADMISSION_TTL;
    let mut turn = Some(turn);
    while Instant::now() < end {
        match listener.accept() {
            Ok((mut socket, _)) => {
                let unused = turn.is_some();
                let _ = serve_one(&mut socket, token, &mut turn);
                if unused && turn.is_none() {
                    end = Instant::now() + Duration::from_secs(2);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(_) => return,
        }
    }
}

fn serve_one(socket: &mut TcpStream, token: &str, turn: &mut Option<Box<Turn>>) -> io::Result<()> {
    socket.set_read_timeout(Some(IO_TIMEOUT))?;
    socket.set_write_timeout(Some(IO_TIMEOUT))?;
    socket.set_nodelay(true)?;
    let mut reader = BufReader::new(socket);
    let mut headers = String::new();
    let mut remaining = MAX_HEADERS;
    loop {
        let start = headers.len();
        let count = reader.by_ref().take(remaining).read_line(&mut headers)? as u64;
        if count == 0 || count == remaining {
            return Ok(());
        }
        remaining -= count;
        if &headers[start..] == "\r\n" {
            break;
        }
    }
    let mut lines = headers.lines();
    let mut request = lines.next().unwrap_or_default().split_ascii_whitespace();
    let path = format!("/relay/{token}/responses");
    let correct_path = request.next() == Some("POST") && request.next() == Some(path.as_str());
    let mut length = None;
    let mut authorized = false;
    let mut isolated = false;
    let bearer = format!("Bearer {token}");
    for (key, value) in lines.filter_map(|line| line.split_once(':')) {
        let value = value.trim();
        if key.eq_ignore_ascii_case("authorization") {
            authorized = value == bearer;
        } else if key.eq_ignore_ascii_case("content-length") {
            length = value.parse::<usize>().ok();
        } else if key.eq_ignore_ascii_case("x-gray-isolated") {
            isolated = value == "1";
        }
    }
    if !correct_path || !authorized {
        return reject(reader.get_mut(), "404 Not Found", "Relay not found");
    }
    if turn.is_none() {
        return reject(
            reader.get_mut(),
            "400 Bad Request",
            gray_acp::relay::ADMISSION_CONSUMED,
        );
    }
    let Some(length) = length else {
        return reject(
            reader.get_mut(),
            "400 Bad Request",
            "Content-Length is required",
        );
    };
    if length > MAX_BODY {
        return reject(
            reader.get_mut(),
            "413 Payload Too Large",
            "Relay body exceeds the limit",
        );
    }
    let mut body = Vec::new();
    reader.by_ref().take(length as u64).read_to_end(&mut body)?;
    let socket = reader.into_inner();
    if body.len() != length {
        return reject(socket, "400 Bad Request", "Incomplete relay body");
    }
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")?;
    let mut stream = Stream(socket);
    if let Some(turn) = turn.take() {
        if let Err(detail) = turn(&body, isolated, &mut stream) {
            stream.event(
                &json!({"type": "response.failed", "response": {"status": "failed",
                "error": {"code": "server_error", "message": detail}}}),
            )?;
        }
    }
    stream.0.write_all(b"data: [DONE]\n\n")
}

fn reject(socket: &mut TcpStream, status: &str, message: &str) -> io::Result<()> {
    let body =
        json!({"type": "error", "error": {"type": "invalid_request_error", "message": message}})
            .to_string();
    write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}
