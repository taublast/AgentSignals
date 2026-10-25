//! What the worker, the HTTP server and the UI share: sessions, rules, notes, the event log.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Shared = std::sync::Arc<std::sync::Mutex<State>>;

/// One live Claude Code session, read from `~/.claude/sessions/<pid>.json`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Session {
    pub pid: u32,
    /// Claude Code's session (conversation) id: `claude --resume <id>` reopens it.
    pub id: String,
    pub name: String,
    /// The user named it (`-n`, `/rename`); otherwise Claude Code made the name up.
    pub named: bool,
    /// A session in a terminal (kind `interactive`, entrypoint `cli`); one-shot `-p` runs, SDK
    /// runs and background workers are never reopened.
    pub interactive: bool,
    /// What its terminal tab shows when it has no name of its own (custom or AI title).
    pub title: String,
    /// `busy`, `idle` or `waiting`.
    pub status: String,
    /// What a `waiting` session waits for (`input needed`, `dialog open`...).
    pub waiting_for: String,
    pub cwd: String,
    /// Epoch ms the status started.
    pub since_ms: u64,
}

/// What a signal does. Text fields take `{name}` and `{waiting}`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Action {
    /// A sound file: a name in the app's sounds folder or the system sounds, or a full path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<String>,
    /// Text the system voice speaks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub say: Option<String>,
    /// Text of a system notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toast: Option<String>,
    /// Toast title (default: the session name, or "Agent Signals").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Flash this app on the taskbar.
    #[serde(default)]
    pub flash: bool,
    /// Bring this app to the front.
    #[serde(default)]
    pub raise: bool,
}

impl Action {
    pub fn is_empty(&self) -> bool {
        self.sound.is_none() && self.say.is_none() && self.toast.is_none() && !self.flash && !self.raise
    }
}

/// "When <session> <on>, do <action>."
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: u32,
    /// Session name, `*` for any.
    pub session: String,
    /// `finished`, `waiting`, `started` or `closed`.
    pub on: String,
    #[serde(flatten)]
    pub action: Action,
    /// Removed after it fired once.
    #[serde(default)]
    pub once: bool,
    /// `finished` only: ignore busy spells shorter than this.
    #[serde(default)]
    pub min_busy_s: f64,
}

pub const EVENTS: [&str; 4] = ["finished", "waiting", "started", "closed"];

impl Rule {
    pub fn matches(&self, name: &str, event: &Event) -> bool {
        (self.session == "*" || self.session.eq_ignore_ascii_case(name))
            && self.on == event.kind
            && (event.kind != "finished" || event.busy_ms as f64 >= self.min_busy_s * 1000.0)
    }
}

/// A status change of a session.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub at_ms: u64,
    pub pid: u32,
    pub session: String,
    /// `finished`, `waiting`, `started` or `closed`.
    pub kind: String,
    /// How long it was busy, for `finished`.
    pub busy_ms: u64,
    /// What the rules did.
    pub fired: String,
}

#[derive(Default)]
pub struct State {
    /// Grows on every change; the UI rebuilds when it moved.
    pub version: u64,
    /// Sorted: waiting, busy, idle, then by name.
    pub sessions: Vec<Session>,
    pub rules: Vec<Rule>,
    /// Newest first.
    pub events: VecDeque<Event>,
    /// Lower-case session name to the line an agent shows under it.
    pub notes: HashMap<String, String>,
    pub muted: bool,
    /// Sessions open last time and closed now (a reboot): the window offers to reopen them.
    pub reopen: Vec<crate::restore::Saved>,
    /// Remembered terminal sessions not running now (closed, or closed by a reboot), last closed first.
    pub inactive: Vec<crate::restore::Saved>,
    /// Lower-case session name to the color its row is tinted with (0xRRGGBB); in settings.json.
    pub tints: HashMap<String, u32>,
}

/// settings.json: what the user set in the window that is not a rule.
#[derive(Default, Serialize, Deserialize)]
struct Settings {
    /// Session name to "#RRGGBB".
    #[serde(default)]
    tints: std::collections::BTreeMap<String, String>,
}

/// "#RRGGBB" (or "RRGGBB") to 0xRRGGBB.
pub fn parse_color(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

pub fn color_text(rgb: u32) -> String {
    format!("#{rgb:06X}")
}

impl State {
    pub fn load() -> State {
        let rules: Vec<Rule> = std::fs::read(rules_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let settings: Settings = std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let tints = settings.tints.iter().filter_map(|(name, c)| Some((name.to_lowercase(), parse_color(c)?))).collect();
        State { rules, tints, ..State::default() }
    }

    /// Tints a session's row (`None`: back to the plain row) and saves it.
    pub fn set_tint(&mut self, name: &str, color: Option<u32>) {
        match color {
            Some(c) => self.tints.insert(name.to_lowercase(), c),
            None => self.tints.remove(&name.to_lowercase()),
        };
        self.version += 1;
        let settings = Settings { tints: self.tints.iter().map(|(n, c)| (n.clone(), color_text(*c))).collect() };
        let path = settings_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if let Err(e) = std::fs::write(&path, serde_json::to_vec_pretty(&settings).unwrap()) {
            eprintln!("saving {}: {e}", path.display());
        }
    }

    pub fn add_rule(&mut self, mut rule: Rule) -> u32 {
        rule.id = self.rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        let id = rule.id;
        self.rules.push(rule);
        self.save_rules();
        id
    }

    pub fn remove_rule(&mut self, id: u32) -> bool {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != id);
        let removed = self.rules.len() != before;
        if removed {
            self.save_rules();
        }
        removed
    }

    pub fn save_rules(&mut self) {
        self.version += 1;
        let path = rules_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        if let Err(e) = std::fs::write(&path, serde_json::to_vec_pretty(&self.rules).unwrap()) {
            eprintln!("saving {}: {e}", path.display());
        }
    }

    pub fn log(&mut self, event: Event) {
        self.events.push_front(event);
        self.events.truncate(50);
        self.version += 1;
    }

    /// The menu's toggles: one signal for when the session finishes, on or off.
    pub fn set_finished_signal(&mut self, name: &str, signal: Signal, on: bool) {
        toggle_finished_signal(&mut self.rules, name, signal, on);
        self.save_rules();
    }

    pub fn note(&self, name: &str) -> &str {
        self.notes.get(&name.to_lowercase()).map_or("", |s| s.as_str())
    }
}

/// The app's own folder: AgentSignals in the per-user app data folder (%APPDATA% on Windows).
pub fn data_dir() -> PathBuf {
    crate::platform::data_dir().join("AgentSignals")
}

fn rules_path() -> PathBuf {
    data_dir().join("rules.json")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

/// A terminal tab title is `<marker> <session name>` (`✳ idle`, `◐ busy`...).
pub fn tab_is(title: &str, name: &str) -> bool {
    title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().eq_ignore_ascii_case(name)
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Status changes between two polls, keyed by PID.
pub fn diff(old: &HashMap<u32, Session>, new: &HashMap<u32, Session>, now: u64) -> Vec<Event> {
    let event = |s: &Session, kind: &str, busy_ms: u64| Event {
        at_ms: now,
        pid: s.pid,
        session: s.name.clone(),
        kind: kind.into(),
        busy_ms,
        fired: String::new(),
    };
    let mut events = Vec::new();
    for (pid, n) in new {
        match old.get(pid) {
            None => events.push(event(n, "started", 0)),
            Some(o) if o.status == n.status => {}
            Some(_) if n.status == "waiting" => events.push(event(n, "waiting", 0)),
            Some(o) if o.status == "busy" && n.status == "idle" => events.push(event(n, "finished", now.saturating_sub(o.since_ms))),
            Some(_) => {}
        }
    }
    for (pid, o) in old {
        if !new.contains_key(pid) {
            events.push(event(o, "closed", 0));
        }
    }
    events.sort_by_key(|e| e.pid);
    events
}

/// A signal the session menu turns on or off for when a session finishes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signal {
    Say,
    Sound,
    Toast,
    Flash,
}

impl Signal {
    fn get(self, a: &Action) -> bool {
        match self {
            Signal::Say => a.say.is_some(),
            Signal::Sound => a.sound.is_some(),
            Signal::Toast => a.toast.is_some(),
            Signal::Flash => a.flash,
        }
    }

    fn set(self, a: &mut Action, on: bool) {
        match self {
            Signal::Say => a.say = on.then(|| "{name} finished".into()),
            Signal::Sound => a.sound = on.then(|| crate::platform::FINISHED_SOUND.into()),
            Signal::Toast => a.toast = on.then(|| "{name} finished".into()),
            Signal::Flash => a.flash = on,
        }
    }

    /// Menu text to turn it on, to turn it off, and its name for screen readers.
    pub fn labels(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Signal::Say => ("🔊 Announce when finished", "🔇 Stop announcing", "Announce when finished"),
            Signal::Sound => ("🔔 Play a sound when finished", "🔕 Stop the sound", "Play a sound when finished"),
            Signal::Toast => ("💬 Notify when finished", "💬 Stop notifying", "Notify when finished"),
            Signal::Flash => ("⚡ Flash taskbar when finished", "⚡ Stop flashing", "Flash taskbar when finished"),
        }
    }
}

fn own_finished(r: &Rule, name: &str) -> bool {
    r.session.eq_ignore_ascii_case(name) && r.on == "finished"
}

/// One of the session's own rules gives this signal when it finishes.
pub fn finished_signal(rules: &[Rule], name: &str, signal: Signal) -> bool {
    rules.iter().any(|r| own_finished(r, name) && signal.get(&r.action))
}

/// On: adds the signal to the session's own `finished` rule, or makes one. Off: takes it out of
/// every such rule and drops the rules left with nothing to do.
pub fn toggle_finished_signal(rules: &mut Vec<Rule>, name: &str, signal: Signal, on: bool) {
    if on {
        if let Some(r) = rules.iter_mut().find(|r| own_finished(r, name) && !r.once) {
            signal.set(&mut r.action, true);
            return;
        }
        let mut action = Action::default();
        signal.set(&mut action, true);
        let id = rules.iter().map(|r| r.id).max().unwrap_or(0) + 1;
        rules.push(Rule { id, session: name.into(), on: "finished".into(), action, once: false, min_busy_s: 0.0 });
        return;
    }
    for r in rules.iter_mut().filter(|r| own_finished(r, name)) {
        signal.set(&mut r.action, false);
    }
    rules.retain(|r| !r.action.is_empty());
}

/// What the rules do for a session, shown after its name: 🔊 speech, 🔔 sound, 💬 toast, ⚡ flash.
pub fn rule_icons(rules: &[Rule], name: &str) -> String {
    let mine: Vec<&Rule> = rules.iter().filter(|r| r.session == "*" || r.session.eq_ignore_ascii_case(name)).collect();
    let mut icons = String::new();
    for (has, icon) in [
        (mine.iter().any(|r| r.action.say.is_some()), "🔊"),
        (mine.iter().any(|r| r.action.sound.is_some()), "🔔"),
        (mine.iter().any(|r| r.action.toast.is_some()), "💬"),
        (mine.iter().any(|r| r.action.flash), "⚡"),
    ] {
        if has {
            icons.push_str(icon);
        }
    }
    icons
}

/// The board's order: waiting, busy, then idle with the one that went idle last on top; by name
/// within. A key, not a comparator: the order is total for any status.
pub fn sort_sessions(list: &mut [Session]) {
    list.sort_by_cached_key(|s| {
        let rank = match s.status.as_str() {
            "waiting" => 0,
            "busy" => 1,
            _ => 2,
        };
        let idle_since = if s.status == "idle" { s.since_ms } else { 0 };
        (rank, std::cmp::Reverse(idle_since), s.name.to_lowercase())
    });
}

/// `{name}` and `{waiting}` filled in.
pub fn expand(text: &str, name: &str, waiting: &str) -> String {
    text.replace("{name}", name).replace("{waiting}", waiting)
}

/// "4m", "2h", "35s".
pub fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(pid: u32, status: &str, since_ms: u64) -> Session {
        Session { pid, id: String::new(), name: format!("s{pid}"), named: true, interactive: true, title: String::new(), status: status.into(), waiting_for: String::new(), cwd: String::new(), since_ms }
    }

    fn map(list: &[Session]) -> HashMap<u32, Session> {
        list.iter().map(|s| (s.pid, s.clone())).collect()
    }

    #[test]
    fn tab_titles() {
        assert!(tab_is("✳ api-server", "api-server"));
        assert!(tab_is("◑ docs-writer", "docs-writer"));
        assert!(tab_is("✳ Fix login redirect loop", "Fix login redirect loop"));
        assert!(!tab_is("✳ api-server", "api-serve"));
    }

    #[test]
    fn transitions() {
        let old = map(&[s(1, "busy", 1_000), s(2, "idle", 0), s(3, "busy", 0), s(4, "idle", 0), s(5, "waiting", 0)]);
        let new = map(&[s(1, "idle", 9_000), s(2, "idle", 0), s(3, "waiting", 0), s(5, "busy", 0), s(6, "idle", 0)]);
        let kinds: Vec<(u32, String, u64)> = diff(&old, &new, 10_000).into_iter().map(|e| (e.pid, e.kind, e.busy_ms)).collect();
        assert_eq!(
            kinds,
            vec![(1, "finished".into(), 9_000), (3, "waiting".into(), 0), (4, "closed".into(), 0), (6, "started".into(), 0)]
        );
    }

    #[test]
    fn rule_matching() {
        let rule = |session: &str, min_busy_s: f64| Rule {
            id: 1,
            session: session.into(),
            on: "finished".into(),
            action: Action::default(),
            once: false,
            min_busy_s,
        };
        let e = Event { at_ms: 0, pid: 1, session: "api-server".into(), kind: "finished".into(), busy_ms: 4_000, fired: String::new() };
        assert!(rule("*", 0.0).matches("api-server", &e));
        assert!(rule("Api-Server", 3.0).matches("api-server", &e));
        assert!(!rule("api-server", 5.0).matches("api-server", &e));
        assert!(!rule("build-bot", 0.0).matches("api-server", &e));
    }

    #[test]
    fn order() {
        let mut list = vec![s(1, "idle", 10), s(2, "busy", 0), s(3, "idle", 30), s(4, "waiting", 0), s(5, "other", 20), s(6, "idle", 30)];
        sort_sessions(&mut list);
        let pids: Vec<u32> = list.iter().map(|s| s.pid).collect();
        assert_eq!(pids, vec![4, 2, 3, 6, 1, 5]);
    }

    #[test]
    fn icons() {
        let rule = |session: &str, on: &str, action: Action| Rule { id: 1, session: session.into(), on: on.into(), action, once: false, min_busy_s: 0.0 };
        let say = Action { say: Some("{name} finished".into()), ..Action::default() };
        let sound = Action { sound: Some("tada".into()), ..Action::default() };
        let rules = vec![rule("api-server", "finished", say), rule("*", "waiting", sound)];
        assert_eq!(rule_icons(&rules, "API-Server"), "🔊🔔");
        assert_eq!(rule_icons(&rules, "build-bot"), "🔔");
        assert!(finished_signal(&rules, "api-server", Signal::Say));
        assert!(!finished_signal(&rules, "build-bot", Signal::Say));
    }

    #[test]
    fn menu_toggles() {
        let mut rules = Vec::new();
        toggle_finished_signal(&mut rules, "api-server", Signal::Say, true);
        toggle_finished_signal(&mut rules, "api-server", Signal::Flash, true);
        assert_eq!(rules.len(), 1, "both signals in one rule");
        assert_eq!(rule_icons(&rules, "api-server"), "🔊⚡");
        toggle_finished_signal(&mut rules, "api-server", Signal::Say, false);
        assert!(!finished_signal(&rules, "api-server", Signal::Say));
        assert!(finished_signal(&rules, "api-server", Signal::Flash));
        toggle_finished_signal(&mut rules, "api-server", Signal::Flash, false);
        assert!(rules.is_empty(), "a rule left with nothing to do goes");
    }
}
