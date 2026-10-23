//! Remembers the open Claude Code sessions, so that they can be reopened after a reboot closed
//! them all: `claude --resume <id>` in Windows Terminal, in their windows and tab order.

use crate::state::{self, Session};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A session as it is reopened.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub id: String,
    /// Its own name for `-n`; empty when Claude Code made the name up.
    pub name: String,
    /// What the window shows for it.
    pub label: String,
    /// The folder it was started in.
    pub cwd: String,
    /// The `--permission-mode` it last ran with; empty for the default.
    pub mode: String,
    /// Its Windows Terminal window (tabs of one window share the number) and tab position.
    pub window: u32,
    pub tab: u32,
    /// Epoch ms it closed while the app was running; 0 = open when the app last saw it, also when
    /// a reboot closed it.
    #[serde(default)]
    pub closed_at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    /// Sessions started in the home folder reopen in this folder instead: Claude Code asks to
    /// trust the home folder every time, a folder trusted once stays trusted. Empty: as started.
    #[serde(default)]
    reopen_home_in: String,
    #[serde(default)]
    sessions: Vec<Saved>,
}

/// A session that disappears counts as closed only this long after: a shutdown ends the sessions
/// one by one before it ends the app, so the ones a reboot closed stay marked open.
const CLOSED_AFTER: Duration = Duration::from_secs(30);

pub struct Memory {
    pub saved: Vec<Saved>,
    pub reopen_home_in: String,
    /// Ids seen running since the app started.
    seen: HashSet<String>,
    /// Ids that stopped running, and when.
    gone: HashMap<String, Instant>,
    /// Ids being reopened, and since when.
    pub reopening: HashMap<String, Instant>,
}

fn path() -> PathBuf {
    state::data_dir().join("sessions.json")
}

impl Memory {
    pub fn load() -> Memory {
        let mut file: File = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // A session without a conversation (closed before its first message) cannot be resumed;
        // the running ones come back with the first poll.
        file.sessions.retain(|s| transcript(&s.id).is_some());
        Memory { saved: file.sessions, reopen_home_in: file.reopen_home_in, seen: HashSet::new(), gone: HashMap::new(), reopening: HashMap::new() }
    }

    pub fn save(&self) {
        let file = File { reopen_home_in: self.reopen_home_in.clone(), sessions: self.saved.clone() };
        let _ = std::fs::create_dir_all(state::data_dir());
        if let Err(e) = std::fs::write(path(), serde_json::to_vec_pretty(&file).unwrap()) {
            eprintln!("saving {}: {e}", path().display());
        }
    }

    /// Remembered sessions that were open when the app last ran and are not running now: what a
    /// reboot closed.
    pub fn closed(&self, live: &[Session]) -> Vec<Saved> {
        self.saved.iter().filter(|s| s.closed_at == 0 && !self.seen.contains(&s.id) && !live.iter().any(|l| l.id == s.id)).cloned().collect()
    }

    /// Every remembered session that is not running, the last closed first.
    pub fn inactive(&self, live: &[Session]) -> Vec<Saved> {
        let mut list: Vec<Saved> = self.saved.iter().filter(|s| !live.iter().any(|l| l.id == s.id)).cloned().collect();
        list.sort_by_cached_key(|s| (std::cmp::Reverse(if s.closed_at == 0 { u64::MAX } else { s.closed_at }), s.label.to_lowercase()));
        list
    }

    /// The running sessions now: new ones are remembered, positions refreshed from `layout`
    /// (window, tab, tab title), stopped ones queued to be forgotten.
    pub fn update(&mut self, live: &[Session], layout: &[(u32, u32, String)]) {
        let now = Instant::now();
        for s in live.iter().filter(|s| !s.id.is_empty() && s.interactive) {
            self.seen.insert(s.id.clone());
            self.gone.remove(&s.id);
            let tab_title = if s.title.is_empty() { &s.name } else { &s.title };
            let place = layout.iter().find(|(_, _, t)| crate::win::tab_is(t, tab_title)).map(|(w, t, _)| (*w, *t));
            let label = if s.named { s.name.clone() } else { tab_title.clone() };
            match self.saved.iter_mut().find(|x| x.id == s.id) {
                Some(x) => {
                    x.closed_at = 0;
                    x.label = label;
                    x.name = if s.named { s.name.clone() } else { String::new() };
                    if let Some((w, t)) = place {
                        (x.window, x.tab) = (w, t);
                    }
                }
                None => {
                    let (cwd, mode) = launch_info(&s.id).unwrap_or_else(|| (s.cwd.clone(), String::new()));
                    let (window, tab) = place.unwrap_or((u32::MAX, u32::MAX));
                    let name = if s.named { s.name.clone() } else { String::new() };
                    self.saved.push(Saved { id: s.id.clone(), name, label, cwd, mode, window, tab, closed_at: 0 });
                }
            }
        }
        for id in &self.seen {
            if !live.iter().any(|l| &l.id == id) {
                self.gone.entry(id.clone()).or_insert(now);
            }
        }
        self.save();
    }

    /// Marks the sessions that stopped `CLOSED_AFTER` ago closed, as of when they stopped.
    pub fn close_expired(&mut self) {
        let now = state::now_ms();
        let expired: Vec<(String, u64)> = self
            .gone
            .iter()
            .filter(|(_, t)| t.elapsed() >= CLOSED_AFTER)
            .map(|(id, t)| (id.clone(), now.saturating_sub(t.elapsed().as_millis() as u64)))
            .collect();
        if expired.is_empty() {
            return;
        }
        for (id, at) in expired {
            self.gone.remove(&id);
            self.seen.remove(&id);
            if let Some(s) = self.saved.iter_mut().find(|s| s.id == id) {
                s.closed_at = at;
            }
        }
        // A session closed before its first message has no conversation to resume.
        self.saved.retain(|s| s.closed_at == 0 || transcript(&s.id).is_some());
        self.save();
    }

    /// Marks sessions closed now: offered after a reboot and declined, they stay in the list.
    pub fn mark_closed(&mut self, ids: &[String]) {
        let now = state::now_ms();
        for s in self.saved.iter_mut().filter(|s| ids.contains(&s.id)) {
            s.closed_at = now;
        }
        self.save();
    }

    pub fn forget(&mut self, ids: &[String]) {
        self.saved.retain(|s| !ids.contains(&s.id));
        for id in ids {
            self.gone.remove(id);
            self.seen.remove(id);
        }
        self.save();
    }
}

/// The transcript of a session, in whichever project folder Claude Code put it.
fn transcript(id: &str) -> Option<PathBuf> {
    let projects = PathBuf::from(std::env::var("USERPROFILE").ok()?).join(".claude").join("projects");
    std::fs::read_dir(projects).ok()?.flatten().map(|d| d.path().join(format!("{id}.jsonl"))).find(|p| p.is_file())
}

/// The folder a session was started in (its first `cwd`) and its last permission mode.
fn launch_info(id: &str) -> Option<(String, String)> {
    let path = transcript(id)?;
    let field = |line: &str, key: &str| serde_json::from_str::<Value>(line).ok()?.get(key)?.as_str().map(str::to_string);
    let mut file = std::fs::File::open(&path).ok()?;
    let cwd = BufReader::new(&file).lines().take(200).map_while(Result::ok).find_map(|l| field(&l, "cwd"))?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(256 * 1024))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    let mode = tail.lines().rev().filter(|l| l.contains("\"permissionMode\"")).find_map(|l| field(l, "permissionMode")).unwrap_or_default();
    Some((cwd, mode))
}

/// The folder a session reopens in: `home_in` for one started in the home folder, when set.
fn start_folder(cwd: &str, home: &str, home_in: &str) -> String {
    if !home_in.is_empty() && cwd.trim_end_matches('\\').eq_ignore_ascii_case(home.trim_end_matches('\\')) {
        home_in.to_string()
    } else {
        cwd.to_string()
    }
}

/// The Windows Terminal command line that reopens one session.
pub fn command_line(s: &Saved, window: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-w", window, "nt", "-d", &s.cwd, "claude", "--resume", &s.id].iter().map(|a| a.to_string()).collect();
    if !s.mode.is_empty() {
        args.extend(["--permission-mode".into(), s.mode.clone()]);
    }
    if !s.name.is_empty() {
        args.extend(["-n".into(), s.name.clone()]);
    }
    args
}

/// Reopens the sessions in their windows and tab order, a few seconds apart, on a thread of its own;
/// sessions started in the home folder in `home_in` when it is set.
pub fn reopen(mut list: Vec<Saved>, home_in: &str) {
    list.sort_by_key(|s| (s.window, s.tab));
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    for s in &mut list {
        s.cwd = start_folder(&s.cwd, &home, home_in);
    }
    std::thread::spawn(move || {
        // Windows get fresh numbers in the order they come, sessions without a known window go last.
        let mut numbers: Vec<u32> = Vec::new();
        for s in &list {
            if !numbers.contains(&s.window) {
                numbers.push(s.window);
            }
            open_tab(s, &format!("agents-{}", numbers.iter().position(|w| *w == s.window).unwrap() + 1));
            std::thread::sleep(Duration::from_secs(3));
        }
    });
}

/// Reopens one session in a new tab of the Windows Terminal window used last.
pub fn reopen_one(mut s: Saved, home_in: &str) {
    s.cwd = start_folder(&s.cwd, &std::env::var("USERPROFILE").unwrap_or_default(), home_in);
    open_tab(&s, "0");
}

fn open_tab(s: &Saved, window: &str) {
    let mut wt = std::process::Command::new("wt");
    wt.args(command_line(s, window));
    // Started from inside a Claude session, the app carries its markers; a session that
    // inherits them runs as that session's child (no transcript saving).
    for (key, _) in std::env::vars().filter(|(k, _)| k.starts_with("CLAUDE")) {
        wt.env_remove(key);
    }
    if let Err(e) = wt.spawn() {
        eprintln!("reopen {}: {e}", s.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_sessions_reopen_elsewhere() {
        assert_eq!(start_folder(r"C:\Users\me", r"C:\Users\me", r"C:\Users\me\Agents"), r"C:\Users\me\Agents");
        assert_eq!(start_folder(r"c:\users\ME\", r"C:\Users\me", r"C:\Users\me\Agents"), r"C:\Users\me\Agents");
        assert_eq!(start_folder(r"C:\Work\repo", r"C:\Users\me", r"C:\Users\me\Agents"), r"C:\Work\repo");
        assert_eq!(start_folder(r"C:\Users\me", r"C:\Users\me", ""), r"C:\Users\me");
    }

    #[test]
    fn inactive_last_closed_first() {
        let saved = |id: &str, closed_at| Saved { id: id.into(), label: id.into(), closed_at, ..Saved::default() };
        let memory = Memory {
            saved: vec![saved("a", 100), saved("live", 0), saved("b", 0), saved("c", 300)],
            reopen_home_in: String::new(),
            seen: HashSet::new(),
            gone: HashMap::new(),
            reopening: HashMap::new(),
        };
        let live = [Session { pid: 1, id: "live".into(), name: "live".into(), named: true, interactive: true, title: String::new(), status: "idle".into(), waiting_for: String::new(), cwd: String::new(), since_ms: 0 }];
        // Open when the app last saw it (a reboot closed it) first, then the last closed.
        let ids: Vec<String> = memory.inactive(&live).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["b", "c", "a"]);
        // Only the one never closed while the app ran is offered after a reboot.
        assert_eq!(memory.closed(&live).len(), 1);
    }
    #[test]
    fn reopen_command() {
        let s = Saved { id: "abc".into(), name: "api-server".into(), cwd: r"C:\Work".into(), mode: "bypassPermissions".into(), ..Saved::default() };
        assert_eq!(
            command_line(&s, "agents-1").join(" "),
            r"-w agents-1 nt -d C:\Work claude --resume abc --permission-mode bypassPermissions -n api-server"
        );
        let unnamed = Saved { id: "abc".into(), cwd: r"C:\Work".into(), ..Saved::default() };
        assert_eq!(command_line(&unnamed, "agents-2").join(" "), r"-w agents-2 nt -d C:\Work claude --resume abc");
    }
}
