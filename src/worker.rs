//! The worker thread: polls Claude Code's session files, turns status changes into events,
//! fires the rules and runs every signal. It never waits on the UI, so signals work while the
//! window is minimized.

use crate::state::{self, Action, Event, Session, Shared};
use crate::win::{self, Signals};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

pub enum Cmd {
    /// Run a signal now; the name fills `{name}` and the toast title.
    Signal(Action, String),
    Focus(String),
    /// Close the session's Windows Terminal tab (confirmed in the UI).
    CloseTab(String),
    Topmost(bool),
}

const POLL: Duration = Duration::from_millis(500);

pub fn start(shared: Shared) -> Sender<Cmd> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("signals".into()).spawn(move || run(shared, rx)).unwrap();
    tx
}

fn sessions_dir() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default()).join(".claude").join("sessions")
}

fn run(shared: Shared, rx: Receiver<Cmd>) {
    let signals = Signals::new();
    let dir = sessions_dir();
    // File -> (modified, parsed); a file is parsed again only when it changed.
    let mut files: HashMap<PathBuf, (SystemTime, Option<Session>)> = HashMap::new();
    let mut live: HashMap<u32, Session> = HashMap::new();
    let mut first = true;
    let mut next_poll = Instant::now();
    loop {
        match rx.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(cmd) => {
                command(&signals, &shared, cmd);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        next_poll = Instant::now() + POLL;
        let now = state::now_ms();
        let mut new = poll(&dir, &mut files);
        for s in new.values_mut() {
            // No statusUpdatedAt in the file: the status started when it was first seen.
            if s.since_ms == 0 {
                s.since_ms = live.get(&s.pid).filter(|o| o.status == s.status).map_or(now, |o| o.since_ms);
            }
        }
        // Sessions already open when the app starts are not "started".
        let events = if first { Vec::new() } else { state::diff(&live, &new, now) };
        first = false;
        let changed = new != live;
        live = new;
        if !changed && events.is_empty() {
            continue;
        }
        let mut st = shared.lock().unwrap();
        let mut sessions: Vec<Session> = live.values().cloned().collect();
        sessions.sort_by(|a, b| {
            rank(&a.status).cmp(&rank(&b.status)).then_with(|| {
                // Idle: a session that just finished goes to the top; the others keep their order.
                if a.status == "idle" && b.status == "idle" { b.since_ms.cmp(&a.since_ms) } else { std::cmp::Ordering::Equal }
            })
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        st.sessions = sessions;
        st.version += 1;
        for mut event in events {
            let waiting = live.get(&event.pid).map_or(String::new(), |s| s.waiting_for.clone());
            let rules: Vec<_> = st.rules.iter().filter(|r| r.matches(&event.session, &event)).cloned().collect();
            let mut fired = Vec::new();
            for rule in &rules {
                fired.push(if st.muted { "muted".into() } else { execute(&signals, &rule.action, &event.session, &waiting) });
                if rule.once {
                    st.remove_rule(rule.id);
                }
            }
            event.fired = fired.join("; ");
            st.log(event);
        }
    }
}

fn rank(status: &str) -> u8 {
    match status {
        "waiting" => 0,
        "busy" => 1,
        _ => 2,
    }
}

fn command(signals: &Signals, shared: &Shared, cmd: Cmd) {
    match cmd {
        Cmd::Signal(action, name) => {
            let muted = shared.lock().unwrap().muted;
            let fired = if muted { "muted".into() } else { execute(signals, &action, &name, "") };
            shared.lock().unwrap().log(Event {
                at_ms: state::now_ms(),
                pid: 0,
                session: name,
                kind: "signal".into(),
                busy_ms: 0,
                fired,
            });
        }
        Cmd::Focus(name) => tab_command(signals, shared, name, "focus", Signals::focus_tab),
        Cmd::CloseTab(name) => tab_command(signals, shared, name, "close", Signals::close_tab),
        Cmd::Topmost(on) => {
            if let Some(hwnd) = win::own_window() {
                win::set_topmost(hwnd, on);
            }
        }
    }
}

/// Runs a terminal tab command for a session and logs how it went. The tab shows the session's
/// title: a session without a name of its own is titled after its custom or AI title, not its
/// registry name (dev-3f).
fn tab_command(signals: &Signals, shared: &Shared, name: String, kind: &str, run: fn(&Signals, &str) -> windows::core::Result<bool>) {
    let title = shared.lock().unwrap().sessions.iter().find(|s| s.name.eq_ignore_ascii_case(&name)).map(|s| s.title.clone());
    let tab = title.filter(|t| !t.is_empty()).unwrap_or_else(|| name.clone());
    let fired = match run(signals, &tab) {
        Ok(true) => "done".into(),
        Ok(false) => "no terminal tab with that title".into(),
        Err(e) => format!("failed: {}", e.message()),
    };
    shared.lock().unwrap().log(Event { at_ms: state::now_ms(), pid: 0, session: name, kind: kind.into(), busy_ms: 0, fired });
}

/// Runs an action; returns what it did, errors included.
fn execute(signals: &Signals, a: &Action, name: &str, waiting: &str) -> String {
    let mut done = Vec::new();
    let mut report = |what: &str, r: windows::core::Result<()>| match r {
        Ok(()) => done.push(what.to_string()),
        Err(e) => done.push(format!("{what} failed: {}", e.message())),
    };
    if let Some(sound) = &a.sound {
        match find_sound(sound) {
            Some(path) => report("sound", signals.sound(&path)),
            None => report("sound", Err(windows::core::Error::new(windows::core::HRESULT(-1), format!("{sound} not found")))),
        }
    }
    if let Some(text) = &a.say {
        report("say", signals.say(&state::expand(text, name, waiting)));
    }
    if let Some(text) = &a.toast {
        let title = a.title.as_deref().map(|t| state::expand(t, name, waiting)).unwrap_or_else(|| {
            if name.is_empty() { "Agent Signals".into() } else { name.into() }
        });
        report("toast", signals.toast(&title, &state::expand(text, name, waiting)));
    }
    if a.flash || a.raise {
        if let Some(hwnd) = win::own_window() {
            if a.raise {
                win::raise(hwnd);
            } else {
                win::flash(hwnd);
            }
        }
        done.push(if a.raise { "raise" } else { "flash" }.into());
    }
    done.join(", ")
}

/// A full path, or a name (".wav" optional) in %APPDATA%\AgentSignals\sounds, then C:\Windows\Media.
pub fn find_sound(sound: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(sound);
    if direct.is_absolute() {
        return direct.is_file().then_some(direct);
    }
    let file = if sound.to_lowercase().ends_with(".wav") { sound.to_string() } else { format!("{sound}.wav") };
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    [state::data_dir().join("sounds"), PathBuf::from(windir).join("Media")].into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Every .wav name the app can play.
pub fn sound_names() -> Vec<String> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let mut names: Vec<String> = [state::data_dir().join("sounds"), PathBuf::from(windir).join("Media")]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")).then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// The live sessions: `<pid>.json` files whose process still runs.
fn poll(dir: &PathBuf, files: &mut HashMap<PathBuf, (SystemTime, Option<Session>)>) -> HashMap<u32, Session> {
    let mut seen = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return seen };
    let mut present = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        present.push(path.clone());
        let parsed = match files.get(&path) {
            Some((m, s)) if *m == modified => s.clone(),
            _ => {
                let s = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| parse(&v));
                files.insert(path.clone(), (modified, s.clone()));
                s
            }
        };
        if let Some(s) = parsed.filter(|s| win::pid_alive(s.pid)) {
            seen.insert(s.pid, s);
        }
    }
    files.retain(|p, _| present.contains(p));
    seen
}

fn parse(v: &Value) -> Option<Session> {
    let pid = v.get("pid")?.as_u64()? as u32;
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let cwd = text("cwd");
    let mut name = text("name");
    if name.is_empty() {
        name = cwd.rsplit(['\\', '/']).next().unwrap_or("").to_string();
    }
    let title = if text("nameSource") == "user" { String::new() } else { transcript_title(&cwd, &text("sessionId")) };
    Some(Session {
        pid,
        name,
        title,
        status: v.get("status").and_then(Value::as_str).unwrap_or("idle").to_string(),
        waiting_for: text("waitingFor"),
        cwd,
        since_ms: v.get("statusUpdatedAt").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// The title a session without a name of its own shows in its terminal tab: its custom title
/// (`/rename`, remote control name), else its AI title. Claude Code appends both to the
/// transcript again and again, so the last 512 KB hold the current ones.
// ponytail: a title older than the last 512 KB is missed; read further back if that shows up.
fn transcript_title(cwd: &str, session_id: &str) -> String {
    use std::io::{Read, Seek, SeekFrom};
    if session_id.is_empty() {
        return String::new();
    }
    let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
        .join(".claude")
        .join("projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"));
    let Ok(mut file) = std::fs::File::open(path) else { return String::new() };
    let len = file.metadata().map_or(0, |m| m.len());
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(512 * 1024)));
    let mut tail = Vec::new();
    let _ = file.read_to_end(&mut tail);
    let tail = String::from_utf8_lossy(&tail);
    let last = |kind: &str, field: &str| {
        tail.lines()
            .rev()
            .filter(|l| l.contains(kind))
            .find_map(|l| serde_json::from_str::<Value>(l).ok()?.get(field)?.as_str().map(str::to_string))
    };
    last("\"custom-title\"", "customTitle").or_else(|| last("\"ai-title\"", "aiTitle")).unwrap_or_default()
}
