//! Agent Signals: a board of the Claude Code sessions running on this machine, and signals
//! (sound, voice, toast, flash) that agents set up through a local HTTP API.

mod http;
mod restore;
mod platform;
mod state;
mod worker;

use drawnui::prelude::*;
use restore::Saved;
use state::{Event, Rule, Session, Shared, Signal, State};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use worker::Cmd;

const TICK_MS: f32 = 1000.0;
/// Blue of the switches when on.
const ACCENT: u32 = 0x3B82F6;
/// The switches: the engine's default width, flatter. Off: the slate of the menu borders, a light
/// thumb; on: the accent blue.
const SWITCH_WIDTH: f32 = 46.0;
const SWITCH_HEIGHT: f32 = 22.0;
const MENU_WIDTH: f32 = 260.0;
/// The window title; the Windows side finds the app's window by it.
pub(crate) const TITLE: &str = "Agent Signals";

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
    /// Labels of the sessions a reboot closed, offered for reopening.
    reopen: Vec<String>,
    /// Terminal sessions not running now: gray rows, a click reopens one.
    inactive: Vec<Saved>,
    /// Lower-case session name to its row tint.
    tints: HashMap<String, u32>,
    /// Ids of sessions that finished unseen: their dots are red.
    unseen: std::collections::HashSet<String>,
    /// The user folder: a session started there shows no folder line.
    home: String,
    /// What the rows were built from: rebuilt only when it changes.
    session_key: (Vec<(u32, String)>, Vec<String>),
    root: Handle<SkiaLayout>,
    session_stack: Handle<SkiaLayout>,
    /// The right-click menu of a session, while open, and the popup window showing it.
    menu: Option<Menu>,
    popup: Option<PopupId>,
    /// The button of the last press on a session row: only the left one focuses its tab.
    button: MouseButton,
    size: Size,
}

struct Menu {
    name: String,
    /// A closed session's id: the menu offers Reopen and Remove.
    closed: Option<String>,
    /// Where it was opened: the palette opens there too.
    at: Point,
    /// "Close session" chosen: the menu asks to confirm.
    confirm: bool,
}

impl App {
    fn session(&self, pid: u32) -> Option<&Session> {
        self.sessions.iter().find(|s| s.pid == pid)
    }

    fn inactive(&self, id: &str) -> Option<&Saved> {
        self.inactive.iter().find(|s| s.id == id)
    }

    fn tint(&self, name: &str) -> Option<u32> {
        self.tints.get(&name.to_lowercase()).copied()
    }
}

fn build(app: &mut App) -> Build<SkiaLayout> {
    SkiaLayer::new().fill().assign(&mut app.root).children((
        SkiaGrid::new().fill().column_definitions("*").row_definitions("Auto, *").row_spacing(0).children((
            // Header: wraps to more lines in a narrow window. The title is on the window frame.
            SkiaWrap::new().fill_x().padding((16, 12, 16, 8)).spacing(12).row(0).children((
                SkiaRow::new().spacing(8).children((
                    SkiaSwitch::new()
                        .color_frame_on(hex(ACCENT))
                        .width_request(SWITCH_WIDTH)
                        .height_request(SWITCH_HEIGHT)
                        .color_frame_off(hex(0x3B4256))
                        .color_thumb_off(hex(0xC9CDD4))
                        .accessibility_label("Keep window on top")
                        .vertical_options(LayoutOptions::Center)
                        .on_toggled(|_me, app: &mut App, _cx, on| {
                            let _ = app.tx.send(Cmd::Topmost(on));
                        }),
                    label("On top", 14).vertical_options(LayoutOptions::Center),
                )),
                SkiaRow::new().spacing(8).children((
                    SkiaSwitch::new()
                        .color_frame_on(hex(ACCENT))
                        .width_request(SWITCH_WIDTH)
                        .height_request(SWITCH_HEIGHT)
                        .color_frame_off(hex(0x3B4256))
                        .color_thumb_off(hex(0xC9CDD4))
                        .accessibility_label("Mute all signals")
                        .vertical_options(LayoutOptions::Center)
                        .observe(|me, app: &App| me.set_is_toggled(app.muted))
                        .on_toggled(|_me, app: &mut App, _cx, on| {
                            // The window's copy too, at once: `observe` sets the switch from it, and a
                            // copy still saying the old value until the next sync would flip it back.
                            app.muted = on;
                            let mut st = app.shared.lock().unwrap();
                            st.muted = on;
                            st.version += 1;
                        }),
                    label("Mute", 14).vertical_options(LayoutOptions::Center),
                )),
                label("", 13).text_color(hex(0x9CA3AF)).vertical_options(LayoutOptions::Center).observe(|me, app: &App| {
                    let count = |s: &str| app.sessions.iter().filter(|x| x.status == s).count();
                    let closed = if app.inactive.is_empty() { String::new() } else { format!(" · {} closed", app.inactive.len()) };
                    me.set_text(format!("{} waiting · {} busy · {} idle{closed}", count("waiting"), count("busy"), count("idle")));
                }),
            )),
            SkiaScroll::new()
                .fill()
                .row(1)
                // A vertical bar that stays visible, in the 12 pt right padding of the rows.
                .scroll_bar(SkiaScrollBar::new().is_draggable(true).thickness(6))
                .scroll_bar_thumb_color(hex(0x4B5563))
                .scroll_bar_track_color(Color::new(0x14FFFFFF))
                .keep_scroll_bars_visible(true)
                .content(SkiaStack::new().padding((12, 0, 12, 16)).spacing(8).children((
                reopen_banner(),
                SkiaStack::new().spacing(6).assign(&mut app.session_stack),
                section("Recent"),
                label("", 12).text_color(hex(0x9CA3AF)).observe(|me, app: &App| me.set_text(recent(app))),
            ))),
        )),
    ))
}

/// After a reboot: the sessions open last time, with a button that reopens them all.
fn reopen_banner() -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(0x1E2A44))
        .stroke_color(hex(ACCENT))
        .stroke_width(1.55)
        .padding((12, 10, 12, 12))
        .fill_x()
        .observe(|me, app: &App| me.set_is_visible(!app.reopen.is_empty()))
        .children(SkiaStack::new().spacing(8).children((
            label("", 14).text_color(Color::WHITE).observe(|me, app: &App| {
                let n = app.reopen.len();
                me.set_text(format!("{n} session{} from last time {} closed", if n == 1 { "" } else { "s" }, if n == 1 { "is" } else { "are" }));
            }),
            label("", 12).text_color(hex(0x9CA3AF)).max_lines(4).observe(|me, app: &App| me.set_text(app.reopen.join(", "))),
            SkiaRow::new().spacing(8).children((
                SkiaButton::new("")
                    .font_size(13)
                    .accessibility_label("Reopen sessions from last time")
                    .background_color(hex(ACCENT))
                    .observe(|me, app: &App| {
                        let n = app.reopen.len();
                        me.set_text(format!("Reopen {n} session{} from last time", if n == 1 { "" } else { "s" }));
                    })
                    .on_tapped(|_me, app: &mut App, _cx| {
                        let _ = app.tx.send(Cmd::Reopen);
                    }),
                SkiaButton::new("Not now").font_size(13).accessibility_label("Not now").background_color(hex(0x3B4256)).on_tapped(|_me, app: &mut App, _cx| {
                    let _ = app.tx.send(Cmd::Dismiss);
                }),
            )),
        )))
}

/// Opens a session's menu where the right click was, in a popup window of its own: it may reach
/// past the app window's edges. The engine closes it on a press outside, Escape, or when another
/// app takes the keyboard; the mouse leaving both windows closes it too.
fn open_menu(app: &mut App, cx: &mut Cx, menu: Menu) {
    let at = menu.at;
    app.menu = Some(menu);
    show_popup(app, cx, menu_panel(), at);
}

/// The menu's 🎨 Tint: the palette replaces the menu, at the same place.
fn open_palette(app: &mut App, cx: &mut Cx) {
    let Some(at) = app.menu.as_ref().map(|m| m.at) else { return };
    show_popup(app, cx, palette_panel(), at);
}

fn show_popup<T: Control + 'static>(app: &mut App, cx: &mut Cx, content: Build<T>, at: Point) {
    if let Some(open) = app.popup.take() {
        cx.close_popup_window(open);
    }
    app.popup = Some(cx.open_popup_window(content, at, PopupWindowOptions { close_on_pointer_leave: true, ..Default::default() }));
}

/// The palette's colors, "None" first, then by hue; every one stays readable under white text.
const TINTS: [(&str, Option<u32>); 9] = [
    ("None", None),
    ("Orange", Some(0xF97316)),
    ("Amber", Some(0xF59E0B)),
    ("Green", Some(0x22C55E)),
    ("Teal", Some(0x14B8A6)),
    ("Blue", Some(0x3B82F6)),
    ("Purple", Some(0x8B5CF6)),
    ("Pink", Some(0xEC4899)),
    ("Slate", Some(0x64748B)),
];
const ROW: u32 = 0x1C2030;
/// The row of a session that needs a look (waiting, working, finished unseen): a little brighter
/// and bluer than the resting row.
const ROW_ACTIVE: u32 = 0x262E4A;
const SWATCH: f32 = 26.0;
const PALETTE_WIDTH: f32 = 9.0 * SWATCH + 8.0 * 6.0 + 2.0 * 12.0 + 8.0;

/// A session row's background: the resting or the active row, with its tint mixed in (30 %, 38 %
/// when active) so text stays readable.
fn row_color(tint: Option<u32>, active: bool) -> Color {
    let (base, amount) = if active { (ROW_ACTIVE, 0.38) } else { (ROW, 0.3) };
    let Some(t) = tint else { return hex(base) };
    let mix = |shift: u32| {
        let (row, tint) = (((base >> shift) & 0xFF) as f32, ((t >> shift) & 0xFF) as f32);
        (row + (tint - row) * amount) as u32
    };
    hex(mix(16) << 16 | mix(8) << 8 | mix(0))
}

/// The tint palette: the session's name, then the colors in one row.
fn palette_panel() -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(8)
        .background_color(hex(0x262B3B))
        .stroke_color(hex(0x3B4256))
        .stroke_width(1.55)
        .width_request(PALETTE_WIDTH)
        .padding((4, 4, 4, 4))
        .children(SkiaStack::new().children((
            label("", 12).text_color(hex(0x9CA3AF)).max_lines(1).padding((12, 4, 12, 4)).observe(|me, app: &App| {
                if let Some(m) = &app.menu {
                    me.set_text(m.name.clone());
                }
            }),
            SkiaShape::new().fill_x().height_request(1).margin((4, 0, 4, 4)).background_color(hex(0x3B4256)),
            SkiaRow::new()
                .spacing(6)
                .padding((12, 6, 12, 10))
                .accessibility_role(Aria::LIST)
                .children(TINTS.iter().map(|&(name, color)| swatch(name, color)).collect::<Vec<_>>()),
        )))
}

/// One color of the palette; a ring marks the session's current tint, a gray one the color under the mouse.
fn swatch(name: &'static str, color: Option<u32>) -> Build<SkiaShape> {
    let ring = move |app: &App, hovered: bool| {
        let current = app.menu.as_ref().is_some_and(|m| app.tint(&m.name) == color);
        if current {
            Color::WHITE
        } else if hovered {
            hex(0x9CA3AF)
        } else {
            Color::TRANSPARENT
        }
    };
    SkiaShape::new()
        .corner_radius(SWATCH / 2.0)
        .width_request(SWATCH)
        .height_request(SWATCH)
        .background_color(color.map_or(hex(ROW), hex))
        .stroke_width(2)
        .accessibility_role(Aria::MENU_ITEM)
        .accessibility_label(name)
        .observe(move |me, app: &App| me.set_stroke_color(ring(app, false)))
        .on_hovered(move |me, app: &mut App, _cx, on| me.set_stroke_color(ring(app, on)))
        .on_tapped(move |_me, app: &mut App, cx| {
            if let Some(m) = app.menu.take() {
                app.shared.lock().unwrap().set_tint(&m.name, color);
                sync(app, cx);
            }
            menu_done(app, cx);
        })
        // "None" is the plain row with a cross on it.
        .children((label(if color.is_none() { "✕" } else { "" }, 12).text_color(hex(0x9CA3AF)).horizontal_options(LayoutOptions::Center).vertical_options(LayoutOptions::Center),))
}

/// After a menu action: an action that is done with the menu (took `app.menu`) closes its popup.
fn menu_done(app: &mut App, cx: &mut Cx) {
    if app.menu.is_none() {
        if let Some(popup) = app.popup.take() {
            cx.close_popup_window(popup);
        }
    }
}

/// The session menu's panel, the content of its popup window.
fn menu_panel() -> Build<SkiaShape> {
    SkiaShape::new()
            .corner_radius(8)
            .background_color(hex(0x262B3B))
            .stroke_color(hex(0x3B4256))
            .stroke_width(1.55)
            .width_request(MENU_WIDTH)
            .padding((4, 4, 4, 4))
            .children(SkiaStack::new().children((
                // Title bar: whose menu this is, one line, so the wrong session is not touched.
                label("", 12)
                    .text_color(hex(0x9CA3AF))
                    .max_lines(1)
                    .padding((12, 4, 12, 4))
                    .observe(|me, app: &App| {
                        if let Some(m) = &app.menu {
                            me.set_text(m.name.clone());
                        }
                    }),
                SkiaShape::new().fill_x().height_request(1).margin((4, 0, 4, 4)).background_color(hex(0x3B4256)),
                SkiaStack::new().observe(|me, app: &App| me.set_is_visible(app.menu.as_ref().is_some_and(|m| m.closed.is_none() && !m.confirm))).children((
                    menu_item("📋", "Copy name", |app, cx| {
                        if let Some(m) = app.menu.take() {
                            cx.set_clipboard(m.name);
                        }
                    }),
                    signal_item(Signal::Say),
                    signal_item(Signal::Sound),
                    signal_item(Signal::Toast),
                    signal_item(Signal::Flash),
                    menu_item("🎨", "Tint", open_palette),
                    menu_item("❌", "Close session", |app, _cx| {
                        if let Some(m) = &mut app.menu {
                            m.confirm = true;
                        }
                    }),
                )),
                // A closed session.
                SkiaStack::new().observe(|me, app: &App| me.set_is_visible(app.menu.as_ref().is_some_and(|m| m.closed.is_some()))).children((
                    menu_item("📋", "Copy name", |app, cx| {
                        if let Some(m) = app.menu.take() {
                            cx.set_clipboard(m.name);
                        }
                    }),
                    menu_item("🔄", "Reopen", |app, _cx| {
                        if let Some(id) = app.menu.take().and_then(|m| m.closed) {
                            let _ = app.tx.send(Cmd::ReopenOne(id));
                        }
                    }),
                    menu_item("🎨", "Tint", open_palette),
                    menu_item("🗑", "Remove", |app, _cx| {
                        if let Some(id) = app.menu.take().and_then(|m| m.closed) {
                            let _ = app.tx.send(Cmd::Forget(id));
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
                                me.set_text(format!("Close the terminal tab of {}? It stays in the list: click it to reopen.", m.name));
                            }
                        }),
                        SkiaRow::new().spacing(8).children((
                            SkiaButton::new("Close tab").font_size(13).width_request(100).background_color(hex(0xDC2626)).on_tapped(|_me, app: &mut App, cx| {
                                if let Some(m) = app.menu.take() {
                                    let _ = app.tx.send(Cmd::CloseTab(m.name));
                                }
                                menu_done(app, cx);
                            }),
                            SkiaButton::new("Cancel").font_size(13).width_request(100).background_color(hex(0x3B4256)).on_tapped(|_me, app: &mut App, cx| {
                                app.menu = None;
                                menu_done(app, cx);
                            }),
                        )),
                    )),
            )))
}

/// A menu toggle: this signal when the session finishes, on or off.
fn signal_item(signal: Signal) -> Build<SkiaShape> {
    let (_, _, name) = signal.labels();
    SkiaShape::new()
        .corner_radius(6)
        .fill_x()
        .padding((12, 9, 12, 9))
        .accessibility_role(Aria::MENU_ITEM)
        .accessibility_label(name)
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_hovered(hover_item)
        .on_tapped(move |_me, app: &mut App, cx| {
            if let Some(m) = app.menu.take() {
                let on = !state::finished_signal(&app.rules, &m.name, signal);
                app.shared.lock().unwrap().set_finished_signal(&m.name, signal, on);
                sync(app, cx);
            }
            menu_done(app, cx);
        })
        .children((label("", 14).observe(move |me, app: &App| {
            if let Some(m) = &app.menu {
                let (turn_on, turn_off, _) = signal.labels();
                me.set_text(if state::finished_signal(&app.rules, &m.name, signal) { turn_off } else { turn_on });
            }
        }),))
}

/// Every menu item shows an icon before its text; screen readers get the text alone.
fn menu_item(icon: &str, text: &str, action: fn(&mut App, &mut Cx)) -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(6)
        .fill_x()
        .padding((12, 9, 12, 9))
        .accessibility_role(Aria::MENU_ITEM)
        .accessibility_label(text)
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_hovered(hover_item)
        .on_tapped(move |_me, app: &mut App, cx| {
            action(app, cx);
            menu_done(app, cx);
        })
        .children((label(&format!("{icon} {text}"), 14),))
}

/// A menu item under the mouse takes the panel's border color.
fn hover_item(me: &mut Mut<'_, SkiaShape>, _app: &mut App, _cx: &mut Cx, on: bool) {
    me.set_background_color(if on { hex(0x3B4256) } else { Color::TRANSPARENT });
}

/// Emoji (status, rule icons, agents' notes) come from the system's emoji font.
fn label(text: &str, size: i32) -> Build<SkiaLabel> {
    SkiaLabel::new(text).font_size(size).text_color(hex(0xE5E7EB)).system_font_fallback(true)
}

fn section(title: &str) -> Build<SkiaLabel> {
    label(title, 13).text_color(hex(0x9CA3AF)).margin((4, 12, 0, 0))
}

/// Idle and closed sessions: the quiet gray.
const IDLE: u32 = 0x6B7280;
/// The dot of a session that finished while its terminal tab was not in front, until seen.
const FINISHED: u32 = 0xEF4444;

fn status_color(status: &str) -> Color {
    match status {
        // Yellow asks you something, green works, gray rests (as closed sessions are gray); red is
        // `FINISHED`, an idle session not looked at yet.
        "waiting" => hex(0xFACC15),
        "busy" => hex(0x22C55E),
        _ => hex(IDLE),
    }
}

/// One session: status dot, name, note / what it waits for / folder, status and for how long.
/// Left click: select its terminal tab. Right click / Menu key: copy its name, close it.
fn session_row(pid: u32, name: String) -> Build<SkiaShape> {
    let (focus, menu, tint) = (name.clone(), name.clone(), name.clone());
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(ROW))
        .observe(move |me, app: &App| {
            // Brighter while it needs a look: not idle, or finished while you looked elsewhere.
            let active = app.session(pid).is_some_and(|s| s.status != "idle" || app.unseen.contains(&s.id));
            me.set_background_color(row_color(app.tint(&tint), active));
        })
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
        .on_context_menu(move |_me, app: &mut App, cx, at| {
            open_menu(app, cx, Menu { name: menu.clone(), closed: None, at: at.location, confirm: false });
            true
        })
        .children((
            SkiaShape::new()
                .corner_radius(5)
                .width_request(10)
                .height_request(10)
                .vertical_options(LayoutOptions::Center)
                .observe(move |me, app: &App| {
                    let Some(s) = app.session(pid) else { return };
                    me.set_background_color(if app.unseen.contains(&s.id) { hex(FINISHED) } else { status_color(&s.status) });
                }),
            SkiaStack::new().margin((22, 0, 84, 0)).spacing(2).children((
                label(&name, 15).text_color(Color::WHITE).observe(move |me, app: &App| {
                    let icons = state::rule_icons(&app.rules, &name);
                    me.set_text(if icons.is_empty() { name.clone() } else { format!("{name}  {icons}") });
                }),
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
                    me.set_text_color(status_color(&s.status));
                    // Idle needs no icon: the gray dot says it.
                    let icon = match s.status.as_str() {
                        "busy" => "⚙ ", // no U+FE0F: without text shaping it only adds an empty advance
                        "waiting" => "✋ ",
                        _ => "",
                    };
                    me.set_text(format!("{icon}{}", state::age(app.now.saturating_sub(s.since_ms))));
                }),
        ))
}

/// A terminal session that is not running: gray, a click reopens it in a terminal tab.
fn closed_row(id: String, name: String) -> Build<SkiaShape> {
    let (reopen, menu, folder, tint) = (id.clone(), id.clone(), id.clone(), name.clone());
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(ROW))
        .observe(move |me, app: &App| me.set_background_color(row_color(app.tint(&tint), false)))
        .padding((12, 10, 12, 10))
        .fill_x()
        .accessibility_role(Aria::BUTTON)
        .accessibility_label(format!("{name}, closed, reopen"))
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .consume_gestures(|_me, app: &mut App, _cx, gesture| {
            app.button = gesture.button;
            false
        })
        .on_tapped(move |_me, app: &mut App, _cx| {
            if app.button == MouseButton::Left {
                let _ = app.tx.send(Cmd::ReopenOne(reopen.clone()));
            }
        })
        .on_context_menu(move |_me, app: &mut App, cx, at| {
            let name = app.inactive(&menu).map_or(String::new(), |s| s.label.clone());
            open_menu(app, cx, Menu { name, closed: Some(menu.clone()), at: at.location, confirm: false });
            true
        })
        .children((
            SkiaShape::new().corner_radius(5).width_request(10).height_request(10).vertical_options(LayoutOptions::Center).background_color(hex(IDLE)),
            SkiaStack::new().margin((22, 0, 84, 0)).spacing(2).children((
                label(&name, 15).text_color(hex(0x9CA3AF)),
                label("", 12).text_color(hex(0x6B7280)).max_lines(2).observe(move |me, app: &App| {
                    let cwd = app.inactive(&folder).map_or("", |s| s.cwd.as_str());
                    let show = !cwd.eq_ignore_ascii_case(&app.home);
                    me.set_is_visible(show);
                    me.set_text(if show { cwd.to_string() } else { String::new() });
                }),
            )),
            label("", 12)
                .text_color(hex(0x6B7280))
                .horizontal_options(LayoutOptions::End)
                .vertical_options(LayoutOptions::Center)
                .observe(move |me, app: &App| {
                    let at = app.inactive(&id).map_or(0, |s| s.closed_at);
                    me.set_text(if at == 0 { "closed".to_string() } else { format!("closed {}", state::age(app.now.saturating_sub(at))) });
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
    sync(app, cx);
    let root = app.root;
    cx.after(root, TICK_MS, tick);
}

/// Copies the shared state when it changed; rebuilds the rows whose list changed.
fn sync(app: &mut App, cx: &mut Cx) {
    let shared = app.shared.clone();
    let st = shared.lock().unwrap();
    if st.version != app.seen {
        app.seen = st.version;
        app.sessions = st.sessions.clone();
        app.rules = st.rules.clone();
        app.events = st.events.iter().take(15).cloned().collect();
        app.notes = st.notes.clone();
        app.muted = st.muted;
        app.reopen = st.reopen.iter().map(|s| s.label.clone()).collect();
        app.inactive = st.inactive.clone();
        app.tints = st.tints.clone();
        app.unseen = st.unseen.clone();
        drop(st);
        let key = (
            app.sessions.iter().map(|s| (s.pid, s.name.clone())).collect::<Vec<_>>(),
            app.inactive.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        );
        if key != app.session_key {
            cx.clear_children(app.session_stack);
            for (pid, name) in &key.0 {
                cx.add_child(app.session_stack, session_row(*pid, name.clone()));
            }
            for s in &app.inactive {
                cx.add_child(app.session_stack, closed_row(s.id.clone(), s.label.clone()));
            }
            app.session_key = key;
        }
    }
}

static STARTED: OnceLock<(Shared, Sender<Cmd>)> = OnceLock::new();

pub fn run() {
    // A release build has no console: a panic would vanish with the process. Keep it in a file.
    std::panic::set_hook(Box::new(|info| {
        let text = format!("--- {} ms\n{info}\n{}\n", state::now_ms(), std::backtrace::Backtrace::force_capture());
        let _ = std::fs::create_dir_all(state::data_dir());
        let log = std::fs::OpenOptions::new().create(true).append(true).open(state::data_dir().join("crash.log"));
        let _ = log.and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
    }));
    let Some(listener) = http::bind() else {
        // Already running: wave from the running one instead. Another program on the port: say so,
        // never quit without a word.
        if !http::poke_running() {
            platform::error_box(&format!(
                "Agent Signals cannot start: another program uses port {}, which the app needs for its API. Close that program and start Agent Signals again.",
                http::PORT
            ));
        }
        return;
    };
    let shared: Shared = Arc::new(Mutex::new(State::load()));
    let tx = worker::start(shared.clone());
    http::start(listener, shared.clone(), tx.clone());
    let _ = STARTED.set((shared, tx));
    drawnui::run_sized(TITLE, 380.0, 640.0, || {
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
            reopen: Vec::new(),
            home: platform::home_dir().to_string_lossy().into_owned(),
            session_key: (Vec::new(), Vec::new()),
            inactive: Vec::new(),
            tints: HashMap::new(),
            unseen: Default::default(),
            root: Handle::default(),
            session_stack: Handle::default(),
            menu: None,
            popup: None,
            button: MouseButton::Left,
            size: Size::default(),
        };
        Box::new(
            Ui::new(app, build)
                .font_bytes("Default", include_bytes!("../assets/OpenSans-Regular.ttf"))
                .background(hex(0x12141C))
                .on_canvas_resized(|app, size, cx| {
                    app.size = size;
                    if !std::mem::replace(&mut app.ticking, true) {
                        let _ = app.tx.send(Cmd::WindowReady);
                        tick(app, cx);
                    }
                }),
        )
    });
}
