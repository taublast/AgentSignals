//! The API agents call: plain HTTP on 127.0.0.1, JSON in and out, one request at a time.

use crate::state::{Action, EVENTS, Rule, Shared};
use crate::worker::{self, Cmd};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::time::Duration;

pub const PORT: u16 = 47801;

const HELP: &str = "Agent Signals API, http://127.0.0.1:47801
GET    /sessions        live Claude Code sessions (name, pid, status busy|idle|waiting, waiting_for, since_ms, cwd, note)
GET    /rules           rules
POST   /rules           {session: name or \"*\", on: finished|waiting|started|closed, sound?, say?, toast?, title?, flash?, raise?, once?, min_busy_s?}
DELETE /rules/<id>      remove a rule
POST   /signal          {sound?, say?, toast?, title?, flash?, raise?, session?}  signal now
POST   /note            {session, text}  a line shown under the session (empty text clears)
POST   /focus           {session}  select its Windows Terminal tab
GET    /events          last 50 status changes and what fired
GET    /sounds          playable .wav names
Text fields take {name} and {waiting}. Sounds: a name from GET /sounds or a full .wav path.
";

/// The API port; `None` when another instance has it.
pub fn bind() -> Option<TcpListener> {
    TcpListener::bind(("127.0.0.1", PORT)).ok()
}

/// Asks the instance already running to come to the front. False when what holds the port is
/// not Agent Signals (another program, or nothing that answers).
pub fn poke_running() -> bool {
    let ask = |request: String| -> String {
        let Ok(mut s) = TcpStream::connect_timeout(&(std::net::Ipv4Addr::LOCALHOST, PORT).into(), Duration::from_secs(2)) else { return String::new() };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
        let mut answer = Vec::new();
        if s.write_all(request.as_bytes()).is_ok() {
            let _ = s.read_to_end(&mut answer);
        }
        String::from_utf8_lossy(&answer).into_owned()
    };
    if !ask("GET / HTTP/1.1\r\n\r\n".into()).contains("Agent Signals API") {
        return false;
    }
    let body = r#"{"raise":true}"#;
    ask(format!("POST /signal HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
    true
}

pub fn start(listener: TcpListener, shared: Shared, tx: Sender<Cmd>) {
    std::thread::Builder::new()
        .name("http".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                if let Err(e) = serve(stream, &shared, &tx) {
                    eprintln!("http: {e}");
                }
            }
        })
        .unwrap();
}

fn serve(mut stream: TcpStream, shared: &Shared, tx: &Sender<Cmd>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length.min(1 << 20)];
    reader.read_exact(&mut body)?;
    let (status, reply) = route(&method, path.split('?').next().unwrap_or(""), &body, shared, tx);
    let (kind, text) = match reply {
        Value::String(s) => ("text/plain; charset=utf-8", s),
        v => ("application/json", v.to_string()),
    };
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())
}

#[derive(Deserialize)]
struct Note {
    session: String,
    #[serde(default)]
    text: String,
}

#[derive(Deserialize)]
struct Target {
    session: String,
}

#[derive(Deserialize)]
struct SignalIn {
    #[serde(flatten)]
    action: Action,
    #[serde(default)]
    session: String,
}

fn route(method: &str, path: &str, body: &[u8], shared: &Shared, tx: &Sender<Cmd>) -> (&'static str, Value) {
    let ok = ("200 OK", json!({"ok": true}));
    let bad = |e: String| ("400 Bad Request", json!({ "error": e }));
    let parse = |b: &[u8]| serde_json::from_slice::<Value>(if b.is_empty() { b"{}" } else { b });
    match (method, path) {
        ("GET", "/" | "/help") => ("200 OK", Value::String(HELP.into())),
        ("GET", "/sessions") => {
            let st = shared.lock().unwrap();
            let list: Vec<Value> = st
                .sessions
                .iter()
                .map(|s| {
                    let mut v = serde_json::to_value(s).unwrap();
                    v["note"] = st.note(&s.name).into();
                    v
                })
                .collect();
            ("200 OK", Value::Array(list))
        }
        ("GET", "/rules") => ("200 OK", serde_json::to_value(&shared.lock().unwrap().rules).unwrap()),
        ("GET", "/events") => ("200 OK", serde_json::to_value(&shared.lock().unwrap().events).unwrap()),
        ("GET", "/sounds") => ("200 OK", json!(worker::sound_names())),
        ("POST", "/rules") => match parse(body).and_then(serde_json::from_value::<Rule>) {
            Err(e) => bad(e.to_string()),
            Ok(rule) if !EVENTS.contains(&rule.on.as_str()) => bad(format!("on must be one of {EVENTS:?}")),
            Ok(rule) if rule.action.is_empty() => bad("a rule needs sound, say, toast, flash or raise".into()),
            Ok(rule) if rule.action.sound.as_deref().is_some_and(|s| worker::find_sound(s).is_none()) => {
                bad(format!("sound {} not found, see GET /sounds", rule.action.sound.unwrap()))
            }
            Ok(rule) => ("200 OK", json!({ "id": shared.lock().unwrap().add_rule(rule) })),
        },
        ("DELETE", p) if p.starts_with("/rules/") => match p["/rules/".len()..].parse::<u32>() {
            Ok(id) if shared.lock().unwrap().remove_rule(id) => ok,
            _ => ("404 Not Found", json!({"error": "no such rule"})),
        },
        ("POST", "/signal") => match parse(body).and_then(serde_json::from_value::<SignalIn>) {
            Err(e) => bad(e.to_string()),
            Ok(s) if s.action.is_empty() => bad("nothing to signal: sound, say, toast, flash or raise".into()),
            Ok(s) if s.action.sound.as_deref().is_some_and(|x| worker::find_sound(x).is_none()) => {
                bad(format!("sound {} not found, see GET /sounds", s.action.sound.unwrap()))
            }
            Ok(s) => {
                let _ = tx.send(Cmd::Signal(s.action, s.session));
                ok
            }
        },
        ("POST", "/note") => match parse(body).and_then(serde_json::from_value::<Note>) {
            Err(e) => bad(e.to_string()),
            Ok(n) => {
                let mut st = shared.lock().unwrap();
                let key = n.session.to_lowercase();
                if n.text.is_empty() {
                    st.notes.remove(&key);
                } else {
                    st.notes.insert(key, n.text);
                }
                st.version += 1;
                ok
            }
        },
        ("POST", "/focus") => match parse(body).and_then(serde_json::from_value::<Target>) {
            Err(e) => bad(e.to_string()),
            Ok(t) => {
                let _ = tx.send(Cmd::Focus(t.session));
                ok
            }
        },
        _ => ("404 Not Found", Value::String(HELP.into())),
    }
}
