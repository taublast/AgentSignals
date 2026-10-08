//! Agent Signals: a board of the Claude Code sessions running on this machine, and signals
//! (sound, voice, toast, flash) that agents set up through a local HTTP API.

mod http;
mod state;
mod win;
mod worker;

use drawnui::prelude::*;
use state::{Event, Rule, Session, Shared, State};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use worker::Cmd;

const HEADER: i32 = 56;
const TICK_MS: f32 = 1000.0;
const MENU_WIDTH: f32 = 240.0;

fn hex(rgb: u32) -> Color {
    Color::new(0xFF00_0000 | rgb)
}

struct App {
    shared: Shared,
    tx: Sender<Cmd>,
    /// `State::version` last copied.
    seen: u64,
    ticking: bool,
    now: u64,
    // Copies of the shared state, read by the observers.
    sessions: Vec<Session>,
    rules: Vec<Rule>,
    events: Vec<Event>,
    notes: HashMap<String, String>,
    muted: bool,
    /// The user folder: a session started there shows no folder line.
    home: String,
    /// What the rows were built from: rebuilt only when it changes.
    session_key: Vec<(u32, String)>,
    rule_key: Vec<(u32, String)>,
    root: Handle<SkiaLayout>,
    session_stack: Handle<SkiaLayout>,
    rule_stack: Handle<SkiaLayout>,
    /// The right-click menu of a session, while open.
    menu: Option<Menu>,
    /// The button of the last press on a session row: only the left one focuses its tab.
    button: MouseButton,
    size: Size,
}

struct Menu {
    name: String,
    x: f32,
    y: f32,
    /// "Close session" chosen: the menu asks to confirm.
    confirm: bool,
}

impl App {
    fn session(&self, pid: u32) -> Option<&Session> {
        self.sessions.iter().find(|s| s.pid == pid)
    }
}

fn build(app: &mut App) -> Build<SkiaLayout> {
    SkiaLayer::new().fill().assign(&mut app.root).children((
        // Header: switches, counts. The title is on the window frame.
        SkiaLayer::new().padding((16, 12, 16, 0)).children((
            SkiaLabel::new("")
                .font_size(13)
                .text_color(hex(0x9CA3AF))
                .horizontal_options(LayoutOptions::End)
                .vertical_options(LayoutOptions::Center)
                .observe(|me, app: &App| {
                    let count = |s: &str| app.sessions.iter().filter(|x| x.status == s).count();
                    me.set_text(format!("{} waiting · {} busy · {} idle", count("waiting"), count("busy"), count("idle")));
                }),
            SkiaRow::new().spacing(8).children((
                SkiaSwitch::new()
                    .accessibility_label("Mute all signals")
                    .vertical_options(LayoutOptions::Center)
                    .observe(|me, app: &App| me.set_is_toggled(app.muted))
                    .on_toggled(|_me, app: &mut App, _cx, on| {
                        let mut st = app.shared.lock().unwrap();
                        st.muted = on;
                        st.version += 1;
                    }),
                label("Mute", 14).margin((0, 0, 16, 0)).vertical_options(LayoutOptions::Center),
                SkiaSwitch::new()
                    .accessibility_label("Keep window on top")
                    .vertical_options(LayoutOptions::Center)
                    .on_toggled(|_me, app: &mut App, _cx, on| {
                        let _ = app.tx.send(Cmd::Topmost(on));
                    }),
                label("On top", 14).vertical_options(LayoutOptions::Center),
            )),
        )),
        SkiaScroll::new().fill().margin((0, HEADER, 0, 0)).content(SkiaStack::new().padding((12, 0, 12, 16)).spacing(8).children((
            SkiaStack::new().spacing(6).assign(&mut app.session_stack),
            section("Rules"),
            SkiaStack::new().spacing(4).assign(&mut app.rule_stack),
            label("", 12).text_color(hex(0x6B7280)).observe(|me, app: &App| {
                me.set_text(if app.rules.is_empty() { "No rules yet: an agent adds them with POST /rules." } else { "" })
            }),
            section("Recent"),
            label("", 12).text_color(hex(0x9CA3AF)).observe(|me, app: &App| me.set_text(recent(app))),
        ))),
        menu_overlay(),
    ))
}

/// The session menu over a dimmed window; a tap outside it closes it.
fn menu_overlay() -> Build<SkiaLayout> {
    SkiaLayer::new()
        .fill()
        .background_color(Color::new(0x5000_0000))
        .observe(|me, app: &App| me.set_is_visible(app.menu.is_some()))
        .on_tapped(|_me, app: &mut App, _cx| app.menu = None)
        .children(SkiaShape::new()
            .corner_radius(8)
            .background_color(hex(0x262B3B))
            .stroke_color(hex(0x3B4256))
            .stroke_width(1.55)
            .width_request(MENU_WIDTH)
            .padding((4, 4, 4, 4))
            // Taps on the panel itself stay in it.
            .on_tapped(|_me, _app: &mut App, _cx| {})
            .observe(|me, app: &App| {
                if let Some(m) = &app.menu {
                    // Kept inside the window.
                    let x = m.x.min(app.size.width - MENU_WIDTH - 8.0).max(8.0);
                    let y = m.y.min(app.size.height - 140.0).max(8.0);
                    me.set_margin(Thickness::new(x, y, 0.0, 0.0));
                }
            })
            .children((
                SkiaStack::new().observe(|me, app: &App| me.set_is_visible(app.menu.as_ref().is_some_and(|m| !m.confirm))).children((
                    menu_item("Copy name", |app, cx| {
                        if let Some(m) = app.menu.take() {
                            cx.set_clipboard(m.name);
                        }
                    }),
                    menu_item("Close session", |app, _cx| {
                        if let Some(m) = &mut app.menu {
                            m.confirm = true;
                        }
                    }),
                )),
                SkiaStack::new()
                    .padding((10, 8, 10, 8))
                    .spacing(10)
                    .observe(|me, app: &App| me.set_is_visible(app.menu.as_ref().is_some_and(|m| m.confirm)))
                    .children((
                        label("", 13).observe(|me, app: &App| {
                            if let Some(m) = &app.menu {
                                me.set_text(format!("Close the terminal tab of {}? Resume later with claude --resume.", m.name));
                            }
                        }),
                        SkiaRow::new().spacing(8).children((
                            SkiaButton::new("Close tab").font_size(13).width_request(100).background_color(hex(0xDC2626)).on_tapped(|_me, app: &mut App, _cx| {
                                if let Some(m) = app.menu.take() {
                                    let _ = app.tx.send(Cmd::CloseTab(m.name));
                                }
                            }),
                            SkiaButton::new("Cancel").font_size(13).width_request(100).background_color(hex(0x3B4256)).on_tapped(|_me, app: &mut App, _cx| app.menu = None),
                        )),
                    )),
            )))
}

fn menu_item(text: &str, action: fn(&mut App, &mut Cx)) -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(6)
        .fill_x()
        .padding((12, 9, 12, 9))
        .accessibility_role(Aria::MENU_ITEM)
        .accessibility_label(text)
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_tapped(move |_me, app: &mut App, cx| action(app, cx))
        .children((label(text, 14),))
}

fn label(text: &str, size: i32) -> Build<SkiaLabel> {
    SkiaLabel::new(text).font_size(size).text_color(hex(0xE5E7EB))
}

fn section(title: &str) -> Build<SkiaLabel> {
    label(title, 13).text_color(hex(0x9CA3AF)).margin((4, 12, 0, 0))
}

fn status_color(status: &str) -> Color {
    match status {
        // Semaphore: red asks you something, yellow works, green is free.
        "waiting" => hex(0xEF4444),
        "busy" => hex(0xFACC15),
        _ => hex(0x22C55E),
    }
}

/// One session: status dot, name, note / what it waits for / folder, status and for how long.
/// Left click: select its terminal tab. Right click / Menu key: copy its name, close it.
fn session_row(pid: u32, name: String) -> Build<SkiaShape> {
    let (focus, menu) = (name.clone(), name.clone());
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(0x1C2030))
        .padding((12, 10, 12, 10))
        .fill_x()
        .accessibility_role(Aria::BUTTON)
        .accessibility_label(name.clone())
        .animation_tapped(SkiaTouchAnimation::Ripple)
        // Every button taps: remember which one pressed.
        .consume_gestures(|_me, app: &mut App, _cx, gesture| {
            app.button = gesture.button;
            false
        })
        .on_tapped(move |_me, app: &mut App, _cx| {
            if app.button == MouseButton::Left {
                let _ = app.tx.send(Cmd::Focus(focus.clone()));
            }
        })
        .on_context_menu(move |_me, app: &mut App, _cx, at| {
            app.menu = Some(Menu { name: menu.clone(), x: at.location.x, y: at.location.y, confirm: false });
            true
        })
        .children((
            SkiaShape::new()
                .corner_radius(5)
                .width_request(10)
                .height_request(10)
                .vertical_options(LayoutOptions::Center)
                .observe(move |me, app: &App| me.set_background_color(status_color(app.session(pid).map_or("", |s| &s.status)))),
            SkiaStack::new().margin((22, 0, 84, 0)).spacing(2).children((
                label(&name, 15).text_color(Color::WHITE),
                label("", 12).text_color(hex(0x9CA3AF)).max_lines(2).observe(move |me, app: &App| {
                    let Some(s) = app.session(pid) else { return };
                    let note = app.notes.get(&s.name.to_lowercase()).map_or("", |n| n.as_str());
                    let text = if !note.is_empty() {
                        note.to_string()
                    } else if !s.waiting_for.is_empty() {
                        s.waiting_for.clone()
                    } else if !s.title.is_empty() {
                        s.title.clone()
                    } else if s.cwd.eq_ignore_ascii_case(&app.home) {
                        String::new()
                    } else {
                        s.cwd.clone()
                    };
                    me.set_is_visible(!text.is_empty());
                    me.set_text(text);
                }),
            )),
            label("", 12)
                .horizontal_options(LayoutOptions::End)
                .vertical_options(LayoutOptions::Center)
                .observe(move |me, app: &App| {
                    let Some(s) = app.session(pid) else { return };
                    me.set_text_color(if s.status == "idle" { hex(0x6B7280) } else { status_color(&s.status) });
                    me.set_text(format!("{} {}", s.status, state::age(app.now.saturating_sub(s.since_ms))));
                }),
        ))
}

fn rule_row(id: u32, text: String) -> Build<SkiaLayout> {
    SkiaLayer::new().children((
        label(&text, 13).margin((0, 0, 90, 0)).vertical_options(LayoutOptions::Center),
        SkiaButton::new("Remove")
            .font_size(12)
            .horizontal_options(LayoutOptions::End)
            .on_tapped(move |_me, app: &mut App, _cx| {
                app.shared.lock().unwrap().remove_rule(id);
            }),
    ))
}

fn recent(app: &App) -> String {
    if app.events.is_empty() {
        return "Nothing yet.".into();
    }
    let lines: Vec<String> = app
        .events
        .iter()
        .map(|e| {
            let ago = state::age(app.now.saturating_sub(e.at_ms));
            let busy = if e.kind == "finished" { format!(" after {}", state::age(e.busy_ms)) } else { String::new() };
            let fired = if e.fired.is_empty() { String::new() } else { format!(" · {}", e.fired) };
            format!("{ago} ago  {} {}{busy}{fired}", e.session, e.kind)
        })
        .collect();
    lines.join("\n")
}

/// Copies the shared state once a second; rows are rebuilt only when the list changed.
fn tick(app: &mut App, cx: &mut Cx) {
    app.now = state::now_ms();
    let shared = app.shared.clone();
    let st = shared.lock().unwrap();
    if st.version != app.seen {
        app.seen = st.version;
        app.sessions = st.sessions.clone();
        app.rules = st.rules.clone();
        app.events = st.events.iter().take(15).cloned().collect();
        app.notes = st.notes.clone();
        app.muted = st.muted;
        drop(st);
        let key: Vec<(u32, String)> = app.sessions.iter().map(|s| (s.pid, s.name.clone())).collect();
        if key != app.session_key {
            cx.clear_children(app.session_stack);
            for (pid, name) in &key {
                cx.add_child(app.session_stack, session_row(*pid, name.clone()));
            }
            app.session_key = key;
        }
        let key: Vec<(u32, String)> = app.rules.iter().map(|r| (r.id, r.describe())).collect();
        if key != app.rule_key {
            cx.clear_children(app.rule_stack);
            for (id, text) in &key {
                cx.add_child(app.rule_stack, rule_row(*id, text.clone()));
            }
            app.rule_key = key;
        }
    }
    let root = app.root;
    cx.after(root, TICK_MS, tick);
}

static STARTED: OnceLock<(Shared, Sender<Cmd>)> = OnceLock::new();

pub fn run() {
    let Some(listener) = http::bind() else {
        // Already running: wave from the running one instead.
        http::poke_running();
        return;
    };
    let shared: Shared = Arc::new(Mutex::new(State::load()));
    let tx = worker::start(shared.clone());
    http::start(listener, shared.clone(), tx.clone());
    let _ = STARTED.set((shared, tx));
    drawnui::run_sized("Agent Signals", 460.0, 820.0, || {
        let (shared, tx) = STARTED.get().unwrap().clone();
        let app = App {
            shared,
            tx,
            seen: u64::MAX,
            ticking: false,
            now: 0,
            sessions: Vec::new(),
            rules: Vec::new(),
            events: Vec::new(),
            notes: HashMap::new(),
            muted: false,
            home: std::env::var("USERPROFILE").unwrap_or_default(),
            session_key: Vec::new(),
            rule_key: Vec::new(),
            root: Handle::default(),
            session_stack: Handle::default(),
            rule_stack: Handle::default(),
            menu: None,
            button: MouseButton::Left,
            size: Size::default(),
        };
        Box::new(
            Ui::new(app, build)
                .font("Default", "assets/OpenSans-Regular.ttf")
                .background(hex(0x12141C))
                .on_key_down(|app, key, _cx| {
                    let close = key.key == "Escape" && app.menu.is_some();
                    if close {
                        app.menu = None;
                    }
                    close
                })
                .on_canvas_resized(|app, size, cx| {
                    app.size = size;
                    if !std::mem::replace(&mut app.ticking, true) {
                        tick(app, cx);
                    }
                }),
        )
    });
}
