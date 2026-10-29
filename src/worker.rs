//! The worker thread: polls Claude Code's session files, turns status changes into events,
//! fires the rules and runs every signal. It never waits on the UI, so signals work while the
//! window is minimized.

use crate::restore::{self, Memory};
use crate::state::{self, Action, Event, Session, Shared};
use crate::platform::{self, Signals};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime};

pub enum Cmd {
    /// Run a signal now; the name fills `{name}` and the toast title.
    Signal(Action, String),
    Focus(String),
    /// Close the session's terminal tab (confirmed in the UI).
    CloseTab(String),
    Topmost(bool),
    /// The app's window exists: settle what the window gets once (no maximize).
    WindowReady,
    /// Reopen the sessions a reboot closed (`State::reopen`).
    Reopen,
    /// Not now: they stay in the list as closed sessions.
    Dismiss,
    /// Reopen one closed session (its id).
    ReopenOne(String),
    /// Remove a closed session from the list (its id).
    Forget(String),
}

/// The tab layout is read again at least this often, to follow tabs moved by hand.
const LAYOUT_EVERY: Duration = Duration::from_secs(300);

const POLL: Duration = Duration::from_millis(500);

pub fn start(shared: Shared) -> Sender<Cmd> {
    let (tx, rx) = channel();
    std::thread::Builder::new().name("signals".into()).spawn({ let tx = tx.clone(); move || run(shared, rx, tx) }).unwrap();
    tx
}

fn sessions_dir() -> PathBuf {
    platform::home_dir().join(".claude").join("sessions")
}

fn run(shared: Shared, rx: Receiver<Cmd>, tx: Sender<Cmd>) {
    let signals = Signals::new();
    let dir = sessions_dir();
    // File -> (modified, parsed); a file is parsed again only when it changed.
    let mut files: HashMap<PathBuf, (SystemTime, Option<Session>)> = HashMap::new();
    let mut live: HashMap<u32, Session> = HashMap::new();
    let mut first = true;
    let mut next_poll = Instant::now();
    let mut memory = Memory::load();
    // The session ids the layout was last read for, and when.
    let mut layout_ids: Vec<String> = Vec::new();
    let mut layout_at = Instant::now();
    loop {
        match rx.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
            Ok(cmd) => {
                command(&signals, &shared, &mut memory, &tx, cmd);
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
        let changed = new != live;
        live = new;
        if changed || !events.is_empty() {
            let mut st = shared.lock().unwrap();
            let mut sessions: Vec<Session> = live.values().cloned().collect();
            state::sort_sessions(&mut sessions);
            st.sessions = sessions;
            st.version += 1;
            // Every session that finishes gets a red dot, until you click it or it works again.
            for event in events.iter().filter(|e| e.kind == "finished") {
                if let Some(s) = live.get(&event.pid).filter(|s| !s.id.is_empty()) {
                    st.unseen.insert(s.id.clone());
                }
            }
            for mut event in events {
                let waiting = live.get(&event.pid).map_or(String::new(), |s| s.waiting_for.clone());
                let tab = live.get(&event.pid).map_or(String::new(), tab_title);
                let rules: Vec<_> = st.rules.iter().filter(|r| r.matches(&event.session, &event)).cloned().collect();
                let mut fired = Vec::new();
                for rule in &rules {
                    fired.push(if st.muted { "muted".into() } else { execute(&signals, &tx, &rule.action, &event.session, &waiting, &tab) });
                    if rule.once {
                        st.remove_rule(rule.id);
                    }
                }
                event.fired = fired.join("; ");
                st.log(event);
            }
        }
        unseen(&shared, &live);
        // After the list is out: reading the terminals' tab layout can be slow (a permission
        // prompt on macOS), and the window must not wait for it.
        remember(&signals, &shared, &mut memory, &live, first, &mut layout_ids, &mut layout_at);
        first = false;
    }
}

/// A finished session's red dot goes when it works again or ends (a click on its row clears it too).
fn unseen(shared: &Shared, live: &HashMap<u32, Session>) {
    let ids: Vec<String> = shared.lock().unwrap().unseen.iter().cloned().collect();
    if ids.is_empty() {
        return;
    }
    let seen: Vec<String> = ids
        .into_iter()
        .filter(|id| live.values().find(|s| &s.id == id).is_none_or(|s| s.status != "idle"))
        .collect();
    if !seen.is_empty() {
        let mut st = shared.lock().unwrap();
        for id in &seen {
            st.unseen.remove(id);
        }
        st.version += 1;
    }
}

/// Keeps the remembered sessions up to date; at start, offers to reopen what a reboot closed.
fn remember(
    signals: &Signals,
    shared: &Shared,
    memory: &mut Memory,
    live: &HashMap<u32, Session>,
    first: bool,
    layout_ids: &mut Vec<String>,
    layout_at: &mut Instant,
) {
    let list: Vec<Session> = live.values().cloned().collect();
    if first {
        let closed = memory.closed(&list);
        if !closed.is_empty() {
            let mut st = shared.lock().unwrap();
            st.reopen = closed;
            st.version += 1;
        }
    }
    let mut ids: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    if first || ids != *layout_ids || layout_at.elapsed() >= LAYOUT_EVERY {
        memory.update(&list, &signals.tab_layout());
        *layout_ids = ids;
        *layout_at = Instant::now();
    }
    memory.close_expired();
    // Offered sessions that got opened some other way leave the offer.
    let mut st = shared.lock().unwrap();
    let before = st.reopen.len();
    st.reopen.retain(|r| !list.iter().any(|s| s.id == r.id));
    if st.reopen.len() != before {
        st.version += 1;
    }
    let inactive = memory.inactive(&list);
    if inactive != st.inactive {
        st.inactive = inactive;
        st.version += 1;
    }
}

/// A session that could not be reopened, and why, in Recent.
fn log_failed(shared: &Shared, label: &str, why: String) {
    shared.lock().unwrap().log(Event { at_ms: state::now_ms(), pid: 0, session: label.into(), kind: "not reopened".into(), busy_ms: 0, fired: why });
}

fn log_reopen(st: &mut state::State, count: usize) {
    st.log(Event { at_ms: state::now_ms(), pid: 0, session: format!("{count} sessions"), kind: "reopened".into(), busy_ms: 0, fired: String::new() });
}

fn command(signals: &Signals, shared: &Shared, memory: &mut Memory, tx: &Sender<Cmd>, cmd: Cmd) {
    match cmd {
        Cmd::Reopen => {
            let mut st = shared.lock().unwrap();
            let list = std::mem::take(&mut st.reopen);
            log_reopen(&mut st, list.len());
            memory.reopening.extend(list.iter().map(|s| (s.id.clone(), Instant::now())));
            let shared = shared.clone();
            restore::reopen(list, &memory.reopen_home_in, move |s, why| log_failed(&shared, &s.label, why));
        }
        Cmd::Dismiss => {
            let mut st = shared.lock().unwrap();
            let ids: Vec<String> = std::mem::take(&mut st.reopen).into_iter().map(|s| s.id).collect();
            st.version += 1;
            memory.mark_closed(&ids);
        }
        Cmd::ReopenOne(id) => {
            // Never a second process on one conversation: not while it runs, not while its tab
            // is still starting (a second click).
            let running = shared.lock().unwrap().sessions.iter().any(|s| s.id == id);
            if running || memory.reopening.get(&id).is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
                return;
            }
            let Some(s) = memory.saved.iter().find(|s| s.id == id).cloned() else { return };
            match restore::reopen_one(s.clone(), &memory.reopen_home_in) {
                Ok(()) => {
                    memory.reopening.insert(id, Instant::now());
                    shared.lock().unwrap().log(Event {
                        at_ms: state::now_ms(),
                        pid: 0,
                        session: s.label,
                        kind: "reopened".into(),
                        busy_ms: 0,
                        fired: String::new(),
                    });
                }
                Err(why) => log_failed(shared, &s.label, why),
            }
        }
        Cmd::Forget(id) => {
            memory.forget(&[id.clone()]);
            let mut st = shared.lock().unwrap();
            st.inactive.retain(|s| s.id != id);
            st.reopen.retain(|s| s.id != id);
            st.version += 1;
        }
        Cmd::Signal(action, name) => {
            let muted = shared.lock().unwrap().muted;
            let tab = shared.lock().unwrap().sessions.iter().find(|s| s.name.eq_ignore_ascii_case(&name)).map_or(String::new(), tab_title);
            let fired = if muted { "muted".into() } else { execute(signals, tx, &action, &name, "", &tab) };
            shared.lock().unwrap().log(Event {
                at_ms: state::now_ms(),
                pid: 0,
                session: name,
                kind: "signal".into(),
                busy_ms: 0,
                fired,
            });
        }
        Cmd::Focus(name) => {
            // Asked for by the user (a row or a notification): seen, also where the tab in front
            // cannot be read.
            {
                let mut st = shared.lock().unwrap();
                let ids: Vec<String> = st.sessions.iter().filter(|s| s.name.eq_ignore_ascii_case(&name)).map(|s| s.id.clone()).collect();
                if ids.iter().any(|id| st.unseen.remove(id)) {
                    st.version += 1;
                }
            }
            let editor = shared.lock().unwrap().sessions.iter().find(|s| s.vscode && s.name.eq_ignore_ascii_case(&name)).map(|s| s.cwd.clone());
            match editor {
                Some(cwd) => {
                    let fired = match platform::focus_editor(&cwd) {
                        Ok(true) => "done".into(),
                        Ok(false) => "no VS Code window with that folder".into(),
                        Err(e) => e,
                    };
                    shared.lock().unwrap().log(Event { at_ms: state::now_ms(), pid: 0, session: name, kind: "focus".into(), busy_ms: 0, fired });
                }
                None => tab_command(signals, shared, name, "focus", Signals::focus_tab),
            }
        }
        Cmd::CloseTab(name) => tab_command(signals, shared, name, "close", Signals::close_tab),
        Cmd::Topmost(on) => platform::set_topmost(on),
        Cmd::WindowReady => platform::no_maximize(),
    }
}

/// Runs a terminal tab command for a session and logs how it went. The tab shows the session's
/// title: a session without a name of its own is titled after its custom or AI title, not its
/// registry name (dev-3f).
fn tab_command(signals: &Signals, shared: &Shared, name: String, kind: &str, run: fn(&Signals, &str) -> Result<bool, String>) {
    let title = shared.lock().unwrap().sessions.iter().find(|s| s.name.eq_ignore_ascii_case(&name)).map(|s| s.title.clone());
    let tab = title.filter(|t| !t.is_empty()).unwrap_or_else(|| name.clone());
    let fired = match run(signals, &tab) {
        Ok(true) => "done".into(),
        Ok(false) => "no terminal tab with that title".into(),
        Err(e) => format!("failed: {e}"),
    };
    shared.lock().unwrap().log(Event { at_ms: state::now_ms(), pid: 0, session: name, kind: kind.into(), busy_ms: 0, fired });
}

/// Runs an action; returns what it did, errors included.
/// The title of a session's terminal tab: its custom or AI title when it has no name of its own.
fn tab_title(s: &Session) -> String {
    if s.title.is_empty() { s.name.clone() } else { s.title.clone() }
}

/// Runs an action for a session whose terminal tab is titled `tab` ("" for none).
fn execute(signals: &Signals, tx: &Sender<Cmd>, a: &Action, name: &str, waiting: &str, tab: &str) -> String {
    let mut done = Vec::new();
    let mut report = |what: &str, r: Result<(), String>| match r {
        Ok(()) => done.push(what.to_string()),
        Err(e) => done.push(format!("{what} failed: {e}")),
    };
    if let Some(sound) = &a.sound {
        match find_sound(sound) {
            Some(path) => report("sound", signals.sound(&path)),
            None => report("sound", Err(format!("{sound} not found"))),
        }
    }
    if let Some(text) = &a.say {
        report("say", signals.say(&state::expand(text, name, waiting)));
    }
    if let Some(text) = &a.toast {
        let title = a.title.as_deref().map(|t| state::expand(t, name, waiting)).unwrap_or_else(|| {
            if name.is_empty() { "Agent Signals".into() } else { name.into() }
        });
        // A click on the toast focuses the session's terminal tab, as a click on its row does.
        let on_click: Option<Box<dyn Fn() + Send>> = (!name.is_empty()).then(|| {
            let (tx, name) = (tx.clone(), name.to_string());
            Box::new(move || {
                let _ = tx.send(Cmd::Focus(name.clone()));
            }) as Box<dyn Fn() + Send>
        });
        report("toast", signals.toast(&title, &state::expand(text, name, waiting), on_click));
    }
    if a.flash {
        // The agent's terminal window; Agent Signals itself when there is none (no session, or a
        // terminal the app does not know).
        if !tab.is_empty() && signals.flash_terminal(tab) {
            done.push("flash terminal".into());
        } else {
            platform::flash_app();
            done.push("flash app".into());
        }
    }
    if a.raise {
        platform::raise_app();
        done.push("raise".into());
    }
    done.join(", ")
}

/// Where sounds are looked up: the app's sounds folder, then the system sounds.
fn sound_dirs() -> Vec<PathBuf> {
    std::iter::once(state::data_dir().join("sounds")).chain(platform::sound_dirs()).collect()
}

fn is_sound(path: &std::path::Path) -> bool {
    path.extension().is_some_and(|x| platform::SOUND_EXTENSIONS.iter().any(|e| x.eq_ignore_ascii_case(e)))
}

/// A full path, or a name (extension optional) in the app's sounds folder, then the system sounds.
pub fn find_sound(sound: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(sound);
    if direct.is_absolute() {
        return direct.is_file().then_some(direct);
    }
    let files: Vec<String> = if is_sound(&direct) { vec![sound.into()] } else { platform::SOUND_EXTENSIONS.iter().map(|e| format!("{sound}.{e}")).collect() };
    sound_dirs().into_iter().flat_map(|d| files.iter().map(move |f| d.join(f))).find(|p| p.is_file())
}

/// Every sound name the app can play.
pub fn sound_names() -> Vec<String> {
    let mut names: Vec<String> = sound_dirs()
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            is_sound(&p).then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
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
        if let Some(s) = parsed.filter(|s| platform::pid_alive(s.pid)) {
            seen.insert(s.pid, s);
        }
    }
    files.retain(|p, _| present.contains(p));
    seen
}

fn parse(v: &Value) -> Option<Session> {
    let pid = v.get("pid")?.as_u64()? as u32;
    // A spare is a background process Claude Code keeps ready for the next job, not a session yet.
    if v.get("spare").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let cwd = text("cwd");
    let mut name = text("name");
    if name.is_empty() {
        name = cwd.rsplit(['\\', '/']).next().unwrap_or("").to_string();
    }
    let mut named = text("nameSource") == "user";
    let mut title = String::new();
    if !named {
        // `/rename` names the session in its transcript, not in this file: that name is the
        // session's name. A title Claude Code made up (AI title) stays a second line.
        match transcript_titles(&cwd, &text("sessionId")) {
            (Some(custom), _) => (name, named) = (custom, true),
            (None, ai) => title = ai.unwrap_or_default(),
        }
    }
    Some(Session {
        pid,
        id: text("sessionId"),
        name,
        named,
        // A terminal session is kind `interactive` with entrypoint `cli`; a one-shot `-p` run is
        // also `interactive`, but with entrypoint `sdk-cli`.
        interactive: matches!(text("kind").as_str(), "interactive" | "") && matches!(text("entrypoint").as_str(), "cli" | ""),
        vscode: text("entrypoint") == "claude-vscode",
        title,
        // Claude Code's statuses: busy, shell (running a shell command, which its own UI shows as
        // working), idle, waiting.
        status: match v.get("status").and_then(Value::as_str).unwrap_or("idle") {
            "shell" => "busy",
            s => s,
        }
        .to_string(),
        waiting_for: text("waitingFor"),
        cwd,
        since_ms: v.get("statusUpdatedAt").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// A session's titles from the end of its transcript: the custom title (`/rename`, which is its
/// name) and the AI title Claude Code made up. Claude Code appends both again and again, so the
/// last 512 KB hold the current ones.
// ponytail: a title older than the last 512 KB is missed; read further back if that shows up.
fn transcript_titles(cwd: &str, session_id: &str) -> (Option<String>, Option<String>) {
    use std::io::{Read, Seek, SeekFrom};
    if session_id.is_empty() {
        return (None, None);
    }
    let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = platform::home_dir()
        .join(".claude")
        .join("projects")
        .join(slug)
        .join(format!("{session_id}.jsonl"));
    let Ok(mut file) = std::fs::File::open(path) else { return (None, None) };
    let len = file.metadata().map_or(0, |m| m.len());
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(512 * 1024)));
    let mut tail = Vec::new();
    let _ = file.read_to_end(&mut tail);
    titles_in(&String::from_utf8_lossy(&tail))
}

/// The last custom title and the last AI title in transcript lines; empty ones count as none.
fn titles_in(tail: &str) -> (Option<String>, Option<String>) {
    let last = |kind: &str, field: &str| {
        tail.lines()
            .rev()
            .filter(|l| l.contains(kind))
            .find_map(|l| serde_json::from_str::<Value>(l).ok()?.get(field)?.as_str().map(str::to_string))
            .filter(|t| !t.trim().is_empty())
    };
    (last("\"custom-title\"", "customTitle"), last("\"ai-title\"", "aiTitle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(json: &str) -> Option<Session> {
        parse(&serde_json::from_str(json).unwrap())
    }

    /// Session files of the kinds Claude Code writes, old and new: none may break the list.
    #[test]
    fn session_files() {
        let s = session(r#"{"pid":1,"sessionId":"a","cwd":"C:\\Work\\shop","name":"api","nameSource":"user","kind":"interactive","entrypoint":"cli","status":"shell","statusUpdatedAt":5}"#).unwrap();
        assert!(s.interactive && s.named);
        assert_eq!((s.name.as_str(), s.status.as_str(), s.since_ms), ("api", "busy", 5));
        // A background job and a one-shot `-p` run are listed, never remembered for reopening.
        assert!(!session(r#"{"pid":2,"sessionId":"b","kind":"bg","entrypoint":"cli","name":"job"}"#).unwrap().interactive);
        assert!(session(r#"{"pid":4,"sessionId":"d","kind":"bg","entrypoint":"cli","name":"d","spare":true}"#).is_none());
        assert!(!session(r#"{"pid":3,"sessionId":"c","kind":"interactive","entrypoint":"sdk-cli"}"#).unwrap().interactive);
        // VS Code's Claude Code panel: listed, no terminal tab, never reopened in a terminal.
        let vs = session(r#"{"pid":5,"sessionId":"e","kind":"interactive","entrypoint":"claude-vscode","name":"e"}"#).unwrap();
        assert!(vs.vscode && !vs.interactive && !s.vscode);
        // An older file: no kind, entrypoint, name or status.
        let old = session(r#"{"pid":4,"sessionId":"d","cwd":"C:\\Work\\shop"}"#).unwrap();
        assert!(old.interactive);
        assert_eq!((old.name.as_str(), old.status.as_str(), old.since_ms), ("shop", "idle", 0));
        // Not a session file.
        assert!(session(r#"{"name":"x"}"#).is_none());
        assert!(session(r#"{"pid":"x"}"#).is_none());
    }

    #[test]
    fn transcript_titles() {
        let lines = [
            r#"{"type":"ai-title","aiTitle":"Fix login loop"}"#,
            r#"{"type":"custom-title","customTitle":"old name"}"#,
            r#"{"type":"user","message":"hi"}"#,
            r#"{"type":"custom-title","customTitle":"filterscam-mac"}"#,
            r#"{"type":"ai-title","aiTitle":"Fix login redirect loop"}"#,
        ]
        .join("\n");
        // The newest of each kind wins.
        assert_eq!(titles_in(&lines), (Some("filterscam-mac".into()), Some("Fix login redirect loop".into())));
        assert_eq!(titles_in(r#"{"type":"ai-title","aiTitle":"Only AI"}"#), (None, Some("Only AI".into())));
        assert_eq!(titles_in(r#"{"type":"custom-title","customTitle":"  "}"#), (None, None));
        assert_eq!(titles_in(""), (None, None));
    }

    /// The sessions folder: missing, or holding broken and foreign files.
    #[test]
    fn sessions_folder() {
        let mut files = HashMap::new();
        let dir = std::env::temp_dir().join(format!("agentsignals-test-{}-sessions", std::process::id()));
        assert!(poll(&dir, &mut files).is_empty());
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::process::id();
        std::fs::write(dir.join(format!("{me}.json")), format!(r#"{{"pid":{me},"sessionId":"a","name":"me","nameSource":"user"}}"#)).unwrap();
        std::fs::write(dir.join("1.json"), "{ not json").unwrap();
        std::fs::write(dir.join("notes.txt"), "hello").unwrap();
        // A file of a process that ended.
        std::fs::write(dir.join("4000000.json"), r#"{"pid":4000000,"sessionId":"z","name":"gone"}"#).unwrap();
        let live = poll(&dir, &mut files);
        assert_eq!(live.len(), 1);
        assert_eq!(live[&me].name, "me");
        let _ = std::fs::remove_dir_all(dir);
    }
}