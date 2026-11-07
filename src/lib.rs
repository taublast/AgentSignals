//! Agent Signals: a board of the Claude Code sessions running on this machine, and signals
//! (sound, voice, toast, flash) that agents set up through a local HTTP API.

mod answers;
mod http;
mod icons;
mod restore;
mod platform;
mod state;
mod theme;
mod turn;
mod update;
mod worker;

use drawnui::prelude::*;
use theme::pal;
use restore::Saved;
use state::{Event, Rule, Session, Shared, Signal, State};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};
use worker::Cmd;

const TICK_MS: f32 = 1000.0;
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
    /// Ids of sessions that finished and were not clicked since: their dots are red.
    unseen: std::collections::HashSet<String>,
    /// A newer release, and the version put off with Later: the banner shows the first unless it is the second.
    update: Option<update::Update>,
    skipped_update: String,
    install: update::Install,
    /// Pinned answers of every session (copy of the shared state).
    pins: Vec<state::Pin>,
    /// The Answers screen while it is open, and the column its entries go in.
    answers: Option<AnswersView>,
    answers_list: Handle<SkiaLayout>,
    /// The window's shell, and its route now: "" the list, "answers", "settings", "help".
    shell: Handle<SkiaShell>,
    route: String,
    /// The list's filter: rows whose name, title, remote id or second line hold it; its field.
    filter: String,
    filter_editor: Handle<SkiaEditor>,
    answers_editor: Handle<SkiaEditor>,
    /// The window on top of others (the header switch; kept across a rebuild).
    topmost: bool,
    /// Copies of the settings, for the Settings screen; `start_at_login` is read from the system.
    theme: String,
    language: String,
    detect_questions: bool,
    check_updates: bool,
    reopen_home_in: String,
    start_at_login: bool,
    /// Why the last setting could not be applied (the start-at-login entry, a missing folder).
    settings_note: String,
    /// The row under the mouse: `p<pid>` for a live session, `c<id>` for a closed one.
    hovered: Option<String>,
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

/// The Answers screen: whose answers, the list once read, the search, the one open.
struct AnswersView {
    id: String,
    name: String,
    /// A running session (Go to session brings its tab forward); else a closed one (it reopens).
    live: bool,
    list: Option<Result<Vec<answers::Answer>, String>>,
    filter: String,
    /// The entries the search lets through, as indexes into `list`: what the recycled cells show.
    shown: Vec<usize>,
    open: Option<usize>,
    /// The entry whose right-click menu is open.
    menu: Option<usize>,
    /// Copy was chosen for the open answer.
    copied: bool,
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
    SkiaLayer::new().fill().assign(&mut app.root).children((content(app),))
}

/// Builds the window's content again under the same root, for a new theme (or language): the old
/// content goes in the same step, on the UI thread; the worker, the API and the update check live
/// outside it. Popups close; the rows, the Answers list and the screen that was open come back.
fn rebuild(app: &mut App, cx: &mut Cx) {
    close_popup(app, cx);
    app.menu = None;
    cx.clear_children(app.root);
    let keep = app.answers.take();
    let content = content(app);
    app.answers = keep;
    cx.add_child(app.root, content);
    // The rows come with the next tick's sync, once the shell has put the new list in the window.
    app.session_key = (Vec::new(), Vec::new());
    app.seen = u64::MAX;
    let route = std::mem::take(&mut app.route);
    if !route.is_empty() {
        cx.go_to(app.shell, &route, false);
    }
}

fn content(app: &mut App) -> Build<SkiaLayout> {
    SkiaLayer::new().fill().background_color(hex(pal().bg)).children((SkiaShell::new()
        .fill()
        .assign(&mut app.shell)
        // Each screen has its own header with Back: no bar of the shell's.
        .nav_bar_height(0.0)
        .page_background_color(hex(pal().bg))
        .use_browser_history(false)
        .on_route_changed(|_me, app: &mut App, _cx, route| {
            app.route = route.to_string();
            if route != "answers" {
                app.answers = None;
            }
        })
        .root(main_page(app))
        .route("answers", |app: &mut App, _: &ShellArguments| answers_page(app))
        .route("settings", |_app: &mut App, _: &ShellArguments| settings_page())
        .route("help", |_app: &mut App, _: &ShellArguments| help_page()),))
}

/// The list: the filter and the header icons, the sessions, the status bar.
fn main_page(app: &mut App) -> Build<SkiaLayout> {
    SkiaGrid::new().fill().column_definitions("*").row_definitions("Auto, *, Auto").row_spacing(0).children((
        SkiaGrid::new().row(0).fill_x().column_definitions("*, Auto").column_spacing(8).padding((12, 12, 10, 8)).children((
            filter_field(app),
            SkiaRow::new().column(1).spacing(2).vertical_options(LayoutOptions::Center).accessibility_role(Aria::TOOLBAR).children((
                icon_button(icons::SETTINGS, "Settings", "Opens the settings", open_settings),
                icon_button(icons::CIRCLE_QUESTION_MARK, "Help", "Opens the help", open_help),
            )),
        )),
        status_bar().row(2),
        SkiaScroll::new()
            .fill()
            .row(1)
            // A vertical bar that stays visible, in the 12 pt right padding of the rows.
            .scroll_bar(SkiaScrollBar::new().is_draggable(true).thickness(6))
            .scroll_bar_thumb_color(hex(pal().scroll_thumb))
            .scroll_bar_track_color(Color::new(pal().scroll_track))
            .keep_scroll_bars_visible(true)
            .content(SkiaStack::new().padding((12, 0, 12, 16)).spacing(8).children((
                reopen_banner(),
                update_banner(),
                // One Tab stop: the arrow keys move between the rows.
                SkiaStack::new().spacing(6).accessibility_role(Aria::LIST).accessibility_label("Sessions").assign(&mut app.session_stack),
                label("", 13).text_color(hex(pal().text_soft)).accessibility_role(Aria::TEXT).observe(|me, app: &App| {
                    let none = !app.filter.trim().is_empty() && !app.sessions.iter().any(|s| shows(app, s)) && !app.inactive.iter().any(|s| shows_closed(app, s));
                    me.set_is_visible(none);
                    me.set_text("No session matches.");
                }),
                section("Recent"),
                label("", 12).text_color(hex(pal().text_soft)).accessibility_role(Aria::TEXT).observe(|me, app: &App| me.set_text(recent(app))),
                // Which build this is, at the very bottom of the list.
                label(concat!("Version ", env!("CARGO_PKG_VERSION")), 12).text_color(hex(pal().muted)).margin((4, 8, 0, 0)).accessibility_role(Aria::TEXT),
            ))),
    ))
}

/// The list's filter: a search icon, then the text, in the dark field of the app's search boxes.
fn filter_field(app: &mut App) -> Build<SkiaShape> {
    search_field("Filter sessions", &mut app.filter_editor, |app| app.filter.clone(), |app, _cx, text| app.filter = text.to_string())
}

/// The app's search box: a search icon, the text, and a small X that clears it while it holds text,
/// in a dark field with the menus' border. `value` is what it shows, `changed` gets each edit (and
/// the empty text from the X).
fn search_field(placeholder: &'static str, editor: &mut Handle<SkiaEditor>, value: fn(&App) -> String, changed: fn(&mut App, &mut Cx, &str)) -> Build<SkiaShape> {
    let mut field_slot = Handle::<SkiaEditor>::default();
    let built = SkiaEditor::new()
        .column(1)
        .fill_x()
        .font_size(13)
        .background_color(Color::TRANSPARENT)
        .stroke_width(0.0)
        .text_color(hex(pal().text))
        .placeholder_color(hex(pal().muted))
        .cursor_color(hex(pal().accent))
        .placeholder_text(placeholder)
        .accessibility_label(placeholder)
        .observe(move |me, app: &App| {
            if !me.is_focused() {
                me.set_text(value(app));
            }
        })
        .on_text_changed(move |_me, app: &mut App, cx, text| changed(app, cx, text))
        .assign(&mut field_slot);
    *editor = field_slot;
    SkiaShape::new()
        .fill_x()
        .corner_radius(8)
        .background_color(hex(pal().row))
        .stroke_color(hex(pal().border))
        .stroke_width(1.55)
        .padding((10, 0, 6, 0))
        .children((SkiaGrid::new().fill_x().column_definitions("Auto, *, Auto").column_spacing(6).children((
            SkiaSvg::from_string(icons::SEARCH).width_request(14).height_request(14).tint_color(hex(pal().muted)).vertical_options(LayoutOptions::Center).input_transparent(true),
            built,
            // Clears the text; there only while it holds some.
            SkiaShape::new()
                .column(2)
                .corner_radius(4)
                .width_request(20)
                .height_request(20)
                .vertical_options(LayoutOptions::Center)
                .accessibility_role(Aria::BUTTON)
                .accessibility_label("Clear")
                .tooltip("Clear")
                .observe(move |me, app: &App| me.set_is_visible(!value(app).is_empty()))
                .on_hovered(|me, _app: &mut App, _cx, on| me.set_background_color(if on { hex(mix(pal().row, pal().hover_toward, pal().hover_control)) } else { Color::TRANSPARENT }))
                .on_tapped(move |_me, app: &mut App, cx| {
                    changed(app, cx, "");
                    if let Some(mut editor) = cx.get_mut(field_slot) {
                        editor.set_text(String::new());
                    }
                })
                .children((SkiaSvg::from_string(icons::X).width_request(12).height_request(12).tint_color(hex(pal().text_soft)).horizontal_options(LayoutOptions::Center).vertical_options(LayoutOptions::Center),)),
        )),))
}

/// The filter's words, each in the session's name, title, remote id or second line (note, what
/// it waits for or asked, folder); never in the chat.
fn shows(app: &App, s: &Session) -> bool {
    let note = app.notes.get(&s.name.to_lowercase()).map_or("", String::as_str);
    matches_filter(&app.filter, &[&s.name, &s.title, &s.remote, &s.waiting_for, note, &s.cwd])
}

fn shows_closed(app: &App, s: &Saved) -> bool {
    matches_filter(&app.filter, &[&s.label, &s.cwd, &s.id])
}

fn matches_filter(filter: &str, fields: &[&str]) -> bool {
    let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    filter.split_whitespace().map(str::to_lowercase).all(|w| fields.iter().any(|f| f.contains(&w)))
}

/// The bottom line: how many sessions wait, asked, work, rest, are closed, each with its dot (none
/// for a count of zero), and the Muted chip at the right while signals are muted.
fn status_bar() -> Build<SkiaLayout> {
    let count = |status: &'static str, word: &'static str| {
        let n = move |app: &App| match status {
            "closed" => app.inactive.len(),
            _ => app.sessions.iter().filter(|s| s.status == status).count(),
        };
        let color = match status {
            "waiting" => Some(pal().waiting),
            "question" => Some(pal().question),
            "busy" => Some(pal().busy),
            "idle" => Some(pal().idle),
            _ => None,
        };
        SkiaRow::new().spacing(5).observe(move |me, app: &App| me.set_is_visible(n(app) > 0)).children((
            SkiaShape::new()
                .corner_radius(3.5)
                .width_request(7)
                .height_request(7)
                .vertical_options(LayoutOptions::Center)
                .background_color(color.map_or(Color::TRANSPARENT, hex))
                .is_visible(color.is_some()),
            label("", 12).text_color(hex(pal().text_soft)).vertical_options(LayoutOptions::Center).observe(move |me, app: &App| me.set_text(format!("{} {word}", n(app)))),
        ))
    };
    // A line on top keeps it apart from the list in both themes.
    SkiaStack::new().fill_x().spacing(0).children((
    SkiaShape::new().fill_x().height_request(1).background_color(hex(pal().border)),
    SkiaGrid::new()
        .fill_x()
        .column_definitions("*, Auto")
        .padding((14, 6, 10, 6))
        .background_color(hex(pal().row))
        .children((
            SkiaWrap::new()
                .spacing(14)
                .vertical_options(LayoutOptions::Center)
                .accessibility_role(Aria::STATUS)
                .observe(|me, app: &App| {
                    let n = |s: &str| app.sessions.iter().filter(|x| x.status == s).count();
                    let parts = [(n("waiting"), "waiting"), (n("question"), "asked"), (n("busy"), "working"), (n("idle"), "idle"), (app.inactive.len(), "closed")];
                    let text: Vec<String> = parts.iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect();
                    me.set_accessibility_label(if text.is_empty() { "No sessions".to_string() } else { text.join(", ") });
                })
                .children((count("waiting", "waiting"), count("question", "asked"), count("busy", "working"), count("idle", "idle"), count("closed", "closed"))),
            // While signals are muted the bar says so; a click turns them back on.
            SkiaShape::new()
                .column(1)
                .corner_radius(6)
                .padding((8, 2, 8, 2))
                .background_color(hex(pal().border))
                .vertical_options(LayoutOptions::Center)
                .accessibility_role(Aria::BUTTON)
                .accessibility_label("Signals are muted. Click to turn them back on.")
                .observe(|me, app: &App| me.set_is_visible(app.muted))
                .on_hovered(|me, _app: &mut App, _cx, on| {
                    me.set_background_color(hex(if on { mix(pal().border, pal().hover_toward, pal().hover_control) } else { pal().border }))
                })
                .on_tapped(|_me, app: &mut App, _cx| set_muted(app, false))
                .children((label("🔇 Muted", 12).text_color(hex(pal().text)),)),
        )),
    ))
}

/// A header icon: a 28 pt square, the icon in the quiet text color, a veil under the mouse. The
/// name shows as its tooltip and is what screen readers read; the hint says what it opens.
fn icon_button(svg: &'static str, name: &str, hint: &str, tap: fn(&mut App, &mut Cx)) -> Build<SkiaShape> {
    let mut icon = Handle::<SkiaSvg>::default();
    let picture = SkiaSvg::from_string(svg).width_request(16).height_request(16).tint_color(hex(pal().text_soft)).horizontal_options(LayoutOptions::Center).vertical_options(LayoutOptions::Center).assign(&mut icon);
    SkiaShape::new()
        .corner_radius(6)
        .width_request(28)
        .height_request(28)
        .vertical_options(LayoutOptions::Center)
        .accessibility_role(Aria::BUTTON)
        .accessibility_label(name)
        .accessibility_hint(hint)
        // Only the icons have a tooltip: they show no text of their own.
        .tooltip(name)
        // Under the mouse the square lights up and the icon takes the bright text color.
        .on_hovered(move |me, _app: &mut App, cx, on| {
            me.set_background_color(if on { hex(mix(pal().bg, pal().hover_toward, pal().hover_control)) } else { Color::TRANSPARENT });
            if let Some(mut icon) = cx.get_mut(icon) {
                icon.set_tint_color(hex(if on { pal().text } else { pal().text_soft }));
            }
        })
        .on_tapped(move |_me, app: &mut App, cx| tap(app, cx))
        .children((picture,))
}

fn open_settings(app: &mut App, cx: &mut Cx) {
    app.start_at_login = platform::starts_at_login();
    app.settings_note.clear();
    open_screen(app, cx, "settings");
}

fn open_help(app: &mut App, cx: &mut Cx) {
    open_screen(app, cx, "help");
}

/// Slides a screen in over the list; from another screen, back to the list first.
fn open_screen(app: &mut App, cx: &mut Cx, route: &str) {
    close_popup(app, cx);
    if app.route == route {
        return;
    }
    if !app.route.is_empty() {
        cx.pop_to_root(app.shell);
    }
    cx.go_to(app.shell, route, true);
}

/// A screen of the shell (Settings, Help): Back and a title, then its content scrolling.
fn screen(title: &str, body: Build<SkiaLayout>) -> Build<SkiaShape> {
    SkiaShape::new()
        .fill()
        .background_color(hex(pal().bg))
        .padding((12, 10, 12, 12))
        .accessibility_role(Aria::DIALOG)
        .accessibility_label(title)
        .children(SkiaGrid::new().fill().column_definitions("*").row_definitions("Auto, *").row_spacing(12).children((
            SkiaGrid::new().fill_x().column_definitions("Auto, *").column_spacing(10).row(0).children((
                back_button(),
                label(title, 16).column(1).text_color(hex(pal().text)).vertical_options(LayoutOptions::Center).accessibility_role(Aria::HEADING),
            )),
            SkiaScroll::new()
                .fill()
                .row(1)
                .scroll_bar(SkiaScrollBar::new().is_draggable(true).thickness(6))
                .scroll_bar_thumb_color(hex(pal().scroll_thumb))
                .scroll_bar_track_color(Color::new(pal().scroll_track))
                .content(body.padding((0, 0, 12, 16))),
        )))
}

/// A section title inside a screen.
fn heading(text: &str) -> Build<SkiaLabel> {
    label(text, 13).text_color(hex(pal().text_soft)).margin((0, 8, 0, 0)).accessibility_role(Aria::HEADING)
}

/// One choice of a group (a theme): the chosen one filled with the accent, the others outlined.
fn chip(text: &str, chosen: impl Fn(&App) -> bool + Clone + 'static, pick: impl Fn(&mut App, &mut Cx) + 'static) -> Build<SkiaShape> {
    let (look, chosen_text) = (chosen.clone(), chosen.clone());
    SkiaShape::new()
        .corner_radius(8)
        .padding((12, 6, 12, 6))
        .stroke_width(1.55)
        .accessibility_role(Aria::RADIO)
        .accessibility_label(text)
        .observe(move |me, app: &App| {
            let on = look(app);
            me.set_background_color(if on { hex(pal().accent) } else { Color::TRANSPARENT });
            me.set_stroke_color(if on { hex(pal().accent) } else { hex(pal().border) });
            me.set_accessibility_is_pressed(Some(on));
        })
        .on_hovered(move |me, app: &mut App, _cx, over| {
            if !chosen(app) {
                me.set_background_color(if over { hex(mix(pal().bg, pal().hover_toward, pal().hover_control)) } else { Color::TRANSPARENT });
            }
        })
        .on_tapped(move |_me, app: &mut App, cx| pick(app, cx))
        .children((label(text, 13).observe(move |me, app: &App| {
            me.set_text_color(if chosen_text(app) { Color::WHITE } else { hex(pal().text_body) });
        }),))
}

/// A setting that is on or off: its name and what it does, the switch at the right.
fn setting_switch(name: &str, about: &str, get: fn(&App) -> bool, set: fn(&mut App, &mut Cx, bool)) -> Build<SkiaLayout> {
    SkiaGrid::new().fill_x().column_definitions("*, Auto").column_spacing(12).children((
        SkiaStack::new().spacing(2).vertical_options(LayoutOptions::Center).children((
            label(name, 14).text_color(hex(pal().text)),
            label(about, 12).text_color(hex(pal().text_soft)),
        )),
        switch(name).column(1).observe(move |me, app: &App| me.set_is_toggled(get(app))).on_toggled(move |_me, app: &mut App, cx, on| {
            if get(app) != on {
                set(app, cx, on);
            }
        }),
    ))
}

/// Mute on or off, at once in the window's copy too: a switch observing it would flip back if the
/// copy still said the old value until the next sync.
fn set_muted(app: &mut App, on: bool) {
    app.muted = on;
    let mut st = app.shared.lock().unwrap();
    st.muted = on;
    st.version += 1;
}

/// The settings saved for the worker, the update check and the next start.
fn save_settings(app: &App) {
    let mut st = app.shared.lock().unwrap();
    st.theme = app.theme.clone();
    st.language = app.language.clone();
    st.detect_questions = app.detect_questions;
    st.check_updates = app.check_updates;
    st.settings_changed();
}

/// Settings: Appearance (theme), Sessions (questions, reopen folder), App (start at login, updates),
/// About.
fn settings_page() -> Build<SkiaShape> {
    let theme = |name: &'static str, text: &'static str| {
        chip(text, move |app: &App| app.theme == name, move |app: &mut App, cx| {
            app.theme = name.to_string();
            save_settings(app);
            theme::set(name);
            rebuild(app, cx);
        })
    };
    let login = if cfg!(windows) { "Start with Windows" } else { "Open at login" };
    screen(
        "Settings",
        SkiaStack::new().spacing(20).children((
            SkiaStack::new().spacing(10).children((
            heading("Appearance"),
            label("Theme", 14).text_color(hex(pal().text)),
            SkiaWrap::new().spacing(8).accessibility_role(Aria::RADIO_GROUP).accessibility_label("Theme").children((
                theme("system", "System"),
                theme("dark", "Dark"),
                theme("light", "Light"),
            )),
            )),
            SkiaStack::new().spacing(10).children((
            heading("Signals"),
            setting_switch(
                "Mute all signals",
                "Silences every sound, voice, notification and flash, for example during a call. Status changes still show under Recent.",
                |app| app.muted,
                |app, _cx, on| set_muted(app, on),
            ),
            )),
            SkiaStack::new().spacing(10).children((
            heading("Sessions"),
            setting_switch(
                "Detect questions",
                "A session whose last answer asks you something turns blue, with the question on its row.",
                |app| app.detect_questions,
                |app, _cx, on| {
                    app.detect_questions = on;
                    save_settings(app);
                },
            ),
            label("Reopen home-folder sessions in", 14).text_color(hex(pal().text)),
            SkiaEditor::new()
                .fill_x()
                .font_size(13)
                .background_color(hex(pal().row))
                .text_color(hex(pal().text))
                .placeholder_color(hex(pal().muted))
                .cursor_color(hex(pal().accent))
                .stroke_color(hex(pal().border))
                .stroke_width(1.55)
                .corner_radius(8)
                .placeholder_text("Their own folder")
                .accessibility_label("Reopen home-folder sessions in")
                .observe(|me, app: &App| {
                    if !me.is_focused() {
                        me.set_text(app.reopen_home_in.clone());
                    }
                })
                .on_text_changed(|_me, app: &mut App, _cx, text| {
                    let folder = text.trim().to_string();
                    if folder.is_empty() || std::path::Path::new(&folder).is_dir() {
                        app.settings_note.clear();
                        app.reopen_home_in = folder.clone();
                        let _ = app.tx.send(Cmd::ReopenFolder(folder));
                    } else {
                        app.settings_note = "No such folder.".into();
                    }
                }),
            label("Claude Code asks to trust the home folder every time; a folder you trusted once does not. Empty: each session reopens in its own folder.", 12).text_color(hex(pal().text_soft)),
            )),
            SkiaStack::new().spacing(10).children((
            heading("App"),
            setting_switch("Keep window on top", "Agent Signals stays above other windows.", |app| app.topmost, |app, _cx, on| {
                app.topmost = on;
                let _ = app.tx.send(Cmd::Topmost(on));
            }),
            setting_switch(login, "Agent Signals starts when you sign in, from where it is now.", |app| app.start_at_login, |app, _cx, on| {
                match platform::set_start_at_login(on) {
                    Ok(()) => {
                        app.start_at_login = on;
                        app.settings_note.clear();
                    }
                    Err(why) => app.settings_note = why,
                }
            }),
            setting_switch(
                "Check for updates",
                "Asks GitHub for a newer version at start and once a day.",
                |app| app.check_updates,
                |app, _cx, on| {
                    app.check_updates = on;
                    save_settings(app);
                },
            ),
            label("", 12).text_color(hex(pal().finished)).accessibility_role(Aria::TEXT).accessibility_live(Aria::LIVE_POLITE).observe(|me, app: &App| {
                me.set_is_visible(!app.settings_note.is_empty());
                me.set_text(app.settings_note.clone());
            }),
            )),
            SkiaStack::new().spacing(10).children((
            heading("About"),
            label(concat!("Agent Signals ", env!("CARGO_PKG_VERSION")), 14).text_color(hex(pal().text)).accessibility_role(Aria::TEXT),
            tool_button(|_| ("↗", "github.com/taublast/AgentSignals")).on_tapped(|_me, _app: &mut App, _cx| platform::open_url("https://github.com/taublast/AgentSignals")),
            )),
        )),
    )
}

/// A small link button, outlined; lighter under the mouse.
fn pill(text: &str) -> Build<SkiaShape> {
    let fill = pal().bg;
    SkiaShape::new()
        .corner_radius(6)
        .padding((12, 4, 12, 4))
        .background_color(hex(fill))
        .stroke_color(hex(pal().border))
        .stroke_width(1.55)
        .accessibility_role(Aria::BUTTON)
        .accessibility_label(text)
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_hovered(move |me, _app: &mut App, _cx, on| me.set_background_color(hex(if on { mix(fill, pal().hover_toward, pal().hover_control) } else { fill })))
        .children((label(text, 13).text_color(hex(pal().text)),))
}

/// A line of Help's color list: the status dot as on a row, then what it means.
fn dot_line(color: u32, text: &str) -> Build<SkiaLayout> {
    SkiaGrid::new().fill_x().column_definitions("Auto, *").column_spacing(10).children((
        SkiaShape::new().corner_radius(5).width_request(10).height_request(10).vertical_options(LayoutOptions::Center).background_color(hex(color)),
        label(text, 13).column(1).text_color(hex(pal().text_body)).accessibility_role(Aria::TEXT),
    ))
}

/// Help: what the colors mean, what clicks and keys do, and where agents talk to the app.
fn help_page() -> Build<SkiaShape> {
    let text = |t: &str| label(t, 13).text_color(hex(pal().text_body)).accessibility_role(Aria::TEXT);
    screen(
        "Help",
        SkiaStack::new().spacing(16).children((
            SkiaStack::new().spacing(6).children((
            heading("The rows"),
            dot_line(pal().waiting, "Claude Code waits for you: a permission dialog or an input prompt."),
            dot_line(pal().question, "The last answer asked you something; the question is on the row."),
            dot_line(pal().busy, "The session is working."),
            dot_line(pal().finished, "It just finished and you have not looked yet."),
            dot_line(pal().idle, "Idle, or closed."),
            )),
            SkiaStack::new().spacing(6).children((
            heading("Clicks"),
            text("Click a row to bring its terminal tab, or its VS Code window, to the front. Click a closed row to reopen it."),
            text("Right-click a row (or the Menu key) for its menu: copy its name, its Answers, signals for when it finishes, a tint, closing it."),
            )),
            SkiaStack::new().spacing(6).children((
            heading("Answers"),
            text("Every question you asked in a session with Claude's answer, newest first. Search them, open one to read or copy it, pin the ones that matter so they stay on top. Right-click an answer to open, pin or copy it."),
            )),
            SkiaStack::new().spacing(6).children((
            heading("Keys"),
            text("Tab and the arrow keys move through the list, Enter clicks, Escape goes back. Ctrl+, opens Settings, F1 this help."),
            )),
            SkiaStack::new().spacing(6).children((
            heading("For agents"),
            text("Claude agents set up signals through the app's local API at http://127.0.0.1:47801; open it in a browser for the list of calls."),
            )),
            SkiaStack::new().spacing(6).children((
            heading("About"),
            text(concat!("Agent Signals ", env!("CARGO_PKG_VERSION"), ", free and open source (MIT).")),
            SkiaRow::new().spacing(4).children((
                text("Built with"),
                tool_button(|_| ("↗", "DrawnUI for Rust")).on_tapped(|_me, _app: &mut App, _cx| platform::open_url("https://github.com/DrawnUi/DrawnUi.Rust")),
            )),
            SkiaRow::new().spacing(8).margin((0, 4, 0, 0)).children((
                pill("GitHub").on_tapped(|_me, _app: &mut App, _cx| platform::open_url("https://github.com/taublast/AgentSignals")),
                pill("Report an issue").on_tapped(|_me, _app: &mut App, _cx| platform::open_url("https://github.com/taublast/AgentSignals/issues/new")),
            )),
            )),
        )),
    )
}

/// A header switch, named for screen readers; under the mouse its track is a little lighter.
fn switch(name: &str) -> Build<SkiaSwitch> {
    let (on, off) = (pal().accent, pal().border);
    SkiaSwitch::new()
        .color_frame_on(hex(on))
        .width_request(SWITCH_WIDTH)
        .height_request(SWITCH_HEIGHT)
        .color_frame_off(hex(off))
        .color_thumb_off(hex(pal().thumb_off))
        .accessibility_label(name)
        .vertical_options(LayoutOptions::Center)
        .on_hovered(move |me, _app: &mut App, _cx, hovered| {
            let lift = |c: u32| hex(if hovered { mix(c, pal().hover_toward, pal().hover_control) } else { c });
            me.set_color_frame_on(Some(lift(on)));
            me.set_color_frame_off(Some(lift(off)));
        })
}


/// A button in the app's colors; under the mouse it is a little lighter, as the switches are.
/// Screen readers and the keyboard take it as a button (`ui`) and read its caption.
fn button(text: &str, color: u32) -> Build<SkiaButton> {
    SkiaButton::new(text).font_size(13).background_color(hex(color)).on_hovered(move |me, _app: &mut App, _cx, hovered| {
        me.set_background_color(hex(if hovered { mix(color, pal().hover_toward, pal().hover_control) } else { color }));
    })
}

/// After a reboot: the sessions open last time, with a button that reopens them all.
fn reopen_banner() -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(pal().banner))
        .stroke_color(hex(pal().accent))
        .stroke_width(1.55)
        .padding((12, 10, 12, 12))
        .fill_x()
        .observe(|me, app: &App| me.set_is_visible(!app.reopen.is_empty()))
        .accessibility_role(Aria::REGION)
        .accessibility_label("Sessions from last time")
        .children(SkiaStack::new().spacing(8).children((
            label("", 14).text_color(hex(pal().text)).accessibility_role(Aria::TEXT).accessibility_live(Aria::LIVE_POLITE).observe(|me, app: &App| {
                let n = app.reopen.len();
                me.set_text(format!("{n} session{} from last time {} closed", if n == 1 { "" } else { "s" }, if n == 1 { "is" } else { "are" }));
            }),
            label("", 12).text_color(hex(pal().text_soft)).max_lines(4).accessibility_role(Aria::TEXT).observe(|me, app: &App| me.set_text(app.reopen.join(", "))),
            SkiaRow::new().spacing(8).children((
                button("", pal().accent)
                    .accessibility_label("Reopen sessions from last time")
                    .observe(|me, app: &App| {
                        let n = app.reopen.len();
                        me.set_text(format!("Reopen {n} session{} from last time", if n == 1 { "" } else { "s" }));
                    })
                    .on_tapped(|_me, app: &mut App, _cx| {
                        let _ = app.tx.send(Cmd::Reopen);
                    }),
                button("Not now", pal().border).on_tapped(|_me, app: &mut App, _cx| {
                    let _ = app.tx.send(Cmd::Dismiss);
                }),
            )),
        )))
}

/// A newer release on GitHub: its download for this system, or Later (no banner for that version again).
fn update_banner() -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(pal().banner))
        .stroke_color(hex(pal().accent))
        .stroke_width(1.55)
        .padding((12, 10, 12, 12))
        .fill_x()
        .observe(|me, app: &App| me.set_is_visible(offered(app).is_some()))
        .accessibility_role(Aria::REGION)
        .accessibility_label("Update")
        .children(SkiaStack::new().spacing(8).children((
            label("", 14).text_color(hex(pal().text)).accessibility_role(Aria::TEXT).accessibility_live(Aria::LIVE_POLITE).observe(|me, app: &App| {
                if let Some(u) = offered(app) {
                    me.set_text(match &app.install {
                        update::Install::Idle => format!("Agent Signals {} is available", u.version),
                        update::Install::Downloading => format!("Downloading Agent Signals {}…", u.version),
                        update::Install::Failed(why) => format!("Agent Signals {} could not be installed: {why}", u.version),
                    });
                }
            }),
            // While it downloads: no buttons. After a failure the browser takes over.
            SkiaRow::new().spacing(8).observe(|me, app: &App| me.set_is_visible(app.install != update::Install::Downloading)).children((
                button("Update and restart", pal().accent)
                    .observe(|me, app: &App| me.set_is_visible(app.install == update::Install::Idle))
                    .on_tapped(|_me, app: &mut App, _cx| {
                        if let Some(u) = offered(app).cloned() {
                            // The window's copy too, at once: the buttons go now, not at the next sync.
                            app.install = update::Install::Downloading;
                            update::install(app.shared.clone(), u);
                        }
                    }),
                button("Download", pal().accent)
                    .observe(|me, app: &App| me.set_is_visible(matches!(app.install, update::Install::Failed(_))))
                    .on_tapped(|_me, app: &mut App, _cx| {
                        if let Some(u) = offered(app) {
                            platform::open_url(&u.url);
                        }
                    }),
                button("Later", pal().border).on_tapped(move |_me, app: &mut App, _cx| {
                    if let Some(version) = offered(app).map(|u| u.version.clone()) {
                        // The window's copy too, at once: the banner goes now, not at the next sync.
                        app.skipped_update = version.clone();
                        app.shared.lock().unwrap().skip_update(&version);
                    }
                }),
            )),
        )))
}

/// Opens the Answers screen for the menu's session and reads its conversation on a thread of
/// its own (a long one is hundreds of MB); the list fills when it is read.
fn open_answers(app: &mut App, cx: &mut Cx) {
    let Some(m) = app.menu.take() else { return };
    let (id, live) = match &m.closed {
        Some(id) => (id.clone(), false),
        None => match app.sessions.iter().find(|s| s.name == m.name) {
            Some(s) => (s.id.clone(), true),
            None => return,
        },
    };
    app.answers = Some(AnswersView { id: id.clone(), name: m.name, live, list: None, filter: String::new(), shown: Vec::new(), open: None, menu: None, copied: false });
    open_screen(app, cx, "answers");
    let shared = app.shared.clone();
    shared.lock().unwrap().answers = None;
    std::thread::spawn(move || {
        let list = answers::read(&id);
        let mut st = shared.lock().unwrap();
        st.answers = Some((id, list));
        st.version += 1;
    });
}

/// The entries the search lets through: the pinned ones first, newest asked first, then the rest
/// newest first; the recycled cells show them again.
fn fill_answers(app: &mut App, cx: &mut Cx) {
    let pins = &app.pins;
    if let Some(view) = &mut app.answers {
        view.shown = match &view.list {
            Some(Ok(list)) => {
                let (mut pinned, rest): (Vec<usize>, Vec<usize>) =
                    answers::matching(list, &view.filter).into_iter().map(|(i, _)| i).partition(|&i| pins.iter().any(|p| p.is(&view.id, &list[i])));
                pinned.sort_by_key(|&i| std::cmp::Reverse(list[i].at_ms));
                pinned.extend(rest);
                pinned
            }
            _ => Vec::new(),
        };
    }
    cx.items_reset(app.answers_list);
}

/// Whether an answer of the open panel's session is pinned.
fn pinned(app: &App, entry: &answers::Answer) -> bool {
    app.answers.as_ref().is_some_and(|view| app.pins.iter().any(|p| p.is(&view.id, entry)))
}

/// A list cell's background: pinned ones carry a hint of the accent; lighter under the mouse.
fn cell_color(pinned: bool, hovered: bool) -> Color {
    row_color(pinned.then_some(pal().accent), false, hovered)
}

/// The entry a recycled cell shows now.
fn cell_entry<'a>(app: &'a App, row: Option<usize>) -> Option<&'a answers::Answer> {
    let view = app.answers.as_ref()?;
    let Some(Ok(list)) = &view.list else { return None };
    list.get(*view.shown.get(row?)?)
}

/// The parts of a recycled entry cell that its bind fills.
#[derive(Default)]
struct AnswerCell {
    cell: Handle<SkiaShape>,
    question: Handle<SkiaLabel>,
    when: Handle<SkiaLabel>,
    answer: Handle<SkiaLabel>,
}

/// One entry: the question and when it was asked on a quiet line, the answer, which is what is
/// looked for, on two brighter lines; a click opens the answer.
fn answer_cell() -> (Build<SkiaShape>, AnswerCell) {
    let mut handles = AnswerCell::default();
    let cell = SkiaShape::new()
        .corner_radius(8)
        .background_color(hex(pal().row))
        .padding((10, 8, 10, 8))
        .fill_x()
        // One bitmap per cell, blitted while scrolling; the ripple is drawn over it.
        .use_cache(CacheType::Image)
        .accessibility_role(Aria::BUTTON)
        .accessibility_hint("Opens the full answer")
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_hovered(|me, app: &mut App, _cx, on| {
            let pinned = cell_entry(app, me.base().context_index).is_some_and(|e| pinned(app, e));
            me.set_background_color(cell_color(pinned, on));
        })
        // Every button taps: remember which one pressed, so a right click only opens the menu.
        .consume_gestures(|_me, app: &mut App, _cx, gesture| {
            app.button = gesture.button;
            false
        })
        .on_tapped(|me, app: &mut App, _cx| {
            if app.button != MouseButton::Left {
                return;
            }
            let row = me.base().context_index;
            if let Some(view) = &mut app.answers {
                view.open = row.and_then(|row| view.shown.get(row).copied());
                view.copied = false;
            }
        })
        .on_context_menu(|me, app: &mut App, cx, at| {
            let row = me.base().context_index;
            let Some(view) = &mut app.answers else { return false };
            view.menu = row.and_then(|row| view.shown.get(row).copied());
            if view.menu.is_some() {
                show_popup(app, cx, answer_menu_panel(), at.location);
            }
            true
        })
        .assign(&mut handles.cell)
        .children(SkiaStack::new().spacing(3).children((
            SkiaGrid::new().fill_x().column_definitions("*, Auto").column_spacing(8).children((
                label("", 12).text_color(hex(pal().text_soft)).max_lines(1).assign(&mut handles.question),
                label("", 12).text_color(hex(pal().muted)).column(1).assign(&mut handles.when),
            )),
            label("", 13).text_color(hex(pal().text)).max_lines(2).assign(&mut handles.answer),
        )));
    (cell, handles)
}

fn bind_answer(cell: &AnswerCell, app: &App, row: usize, cx: &mut Cx) {
    let Some(view) = &app.answers else { return };
    let Some(Ok(list)) = &view.list else { return };
    let Some(entry) = view.shown.get(row).and_then(|&i| list.get(i)) else { return };
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let (question, answer) = (flat(&entry.question), flat(&entry.answer));
    let is_pinned = pinned(app, entry);
    if let Some(mut me) = cx.get_mut(cell.cell) {
        me.set_background_color(cell_color(is_pinned, false));
        // The answer first, as on screen; then the question it answers.
        let when = asked(app, entry);
        let pin = if is_pinned { "Pinned. " } else { "" };
        me.set_accessibility_label(if when.is_empty() { format!("{pin}{answer} Question: {question}") } else { format!("{pin}{answer} Asked {when}: {question}") });
    }
    if let Some(mut me) = cx.get_mut(cell.question) {
        me.set_text(if is_pinned { format!("📌 {question}") } else { question });
    }
    if let Some(mut me) = cx.get_mut(cell.when) {
        me.set_text(asked(app, entry));
    }
    if let Some(mut me) = cx.get_mut(cell.answer) {
        // While searching: the passage with the hit, so it is seen why the entry is listed.
        let hit = Some(view.filter.as_str()).filter(|f| !f.trim().is_empty()).and_then(|f| answers::snippet(&entry.answer, f));
        me.set_text(hit.unwrap_or(answer));
    }
}

/// When a question was asked: "5m ago", empty when the transcript has no time for it.
fn asked(app: &App, entry: &answers::Answer) -> String {
    if entry.at_ms == 0 { String::new() } else { format!("{} ago", state::age(app.now.saturating_sub(entry.at_ms))) }
}

/// The open answer, if any.
fn opened(app: &App) -> Option<&answers::Answer> {
    let view = app.answers.as_ref()?;
    let Some(Ok(list)) = &view.list else { return None };
    list.get(view.open?)
}

/// Answers: a screen over the whole window. A header with Back and whose answers they are; then
/// the search and the list, or one answer in full under a toolbar (Pin, Copy, Go to session).
fn answers_page(app: &mut App) -> Build<SkiaShape> {
    let listing = |app: &App| app.answers.as_ref().is_some_and(|v| v.open.is_none());
    SkiaShape::new()
        .fill()
        .background_color(hex(pal().bg))
        .padding((12, 10, 12, 12))
        .observe(|me, app: &App| {
            if let Some(view) = &app.answers {
                me.set_accessibility_label(format!("Answers of {}", view.name));
            }
        })
        .accessibility_role(Aria::DIALOG)
        .children(SkiaGrid::new().fill().column_definitions("*").row_definitions("Auto, Auto, *").row_spacing(10).children((
            // Header: Back, then the title and the session.
            SkiaGrid::new().fill_x().column_definitions("Auto, *").column_spacing(10).row(0).children((
                back_button(),
                SkiaStack::new().column(1).spacing(0).vertical_options(LayoutOptions::Center).children((
                    label("Answers", 16).text_color(hex(pal().text)).accessibility_role(Aria::HEADING),
                    label("", 12).text_color(hex(pal().text_soft)).max_lines(1).observe(|me, app: &App| {
                        if let Some(view) = &app.answers {
                            me.set_text(view.name.clone());
                        }
                    }),
                )),
            )),
            // The list's search, or the open answer's toolbar.
            search_field(
                "Search questions and answers",
                &mut app.answers_editor,
                |app| app.answers.as_ref().map_or(String::new(), |v| v.filter.clone()),
                |app, cx, text| {
                    if let Some(view) = &mut app.answers {
                        view.filter = text.to_string();
                    }
                    fill_answers(app, cx);
                },
            )
            .row(1)
            .observe(move |me, app: &App| me.set_is_visible(listing(app))),
            SkiaRow::new()
                .row(1)
                .spacing(4)
                .accessibility_role(Aria::TOOLBAR)
                .accessibility_label("Answer")
                .observe(|me, app: &App| me.set_is_visible(opened(app).is_some()))
                .children((
                    tool_button(|app| if opened(app).is_some_and(|e| pinned(app, e)) { ("📌", "Unpin") } else { ("📌", "Pin") }).on_tapped(|_me, app: &mut App, cx| {
                        if let Some(index) = app.answers.as_ref().and_then(|v| v.open) {
                            toggle_pin(app, cx, index);
                        }
                    }),
                    tool_button(|app| if app.answers.as_ref().is_some_and(|v| v.copied) { ("✓", "Copied") } else { ("📋", "Copy") }).on_tapped(|_me, app: &mut App, cx| {
                        if let Some(index) = app.answers.as_ref().and_then(|v| v.open) {
                            copy_answer(app, cx, index);
                            if let Some(view) = &mut app.answers {
                                view.copied = true;
                            }
                        }
                    }),
                    tool_button(|_| ("↗", "Go to session")).on_tapped(|_me, app: &mut App, cx| {
                        if let Some(view) = app.answers.take() {
                            close_popup(app, cx);
                            cx.go_back(app.shell, true);
                            let _ = app.tx.send(if view.live { Cmd::Focus(view.name) } else { Cmd::ReopenOne(view.id) });
                        }
                    }),
                )),
            // The list, or why there is none.
            SkiaLayer::new().fill().row(2).observe(move |me, app: &App| me.set_is_visible(listing(app))).children((
                label("", 13).text_color(hex(pal().text_soft)).accessibility_role(Aria::TEXT).observe(|me, app: &App| {
                    let Some(view) = &app.answers else { return };
                    let text = match &view.list {
                        None => "Reading the conversation…".to_string(),
                        Some(Err(why)) => format!("The conversation could not be read: {why}"),
                        Some(Ok(list)) if list.is_empty() => "No answers yet.".to_string(),
                        Some(Ok(list)) if answers::matching(list, &view.filter).is_empty() => "Nothing matches.".to_string(),
                        Some(Ok(_)) => String::new(),
                    };
                    me.set_is_visible(!text.is_empty());
                    me.set_text(text);
                }),
                SkiaScroll::new()
                    .fill()
                    .scroll_bar(SkiaScrollBar::new().is_draggable(true).thickness(6))
                    .scroll_bar_thumb_color(hex(pal().scroll_thumb))
                    .scroll_bar_track_color(Color::new(pal().scroll_track))
                    .content(
                        // Recycled cells, measured as they come into view: rows differ in height.
                        SkiaStack::new()
                            .recycling_template(RecyclingTemplate::Enabled)
                            .measure_items_strategy(MeasuringStrategy::MeasureVisible)
                            .spacing(6)
                            .padding((0, 0, 10, 0))
                            .accessibility_role(Aria::LIST)
                            .accessibility_label("Questions and answers")
                            .assign(&mut app.answers_list)
                            .items(|app: &App| app.answers.as_ref().map_or(0, |v| v.shown.len()), answer_cell, bind_answer),
                    ),
            )),
            // One answer in full: when, the question above it, the text selectable.
            SkiaScroll::new()
                .fill()
                .row(2)
                .scroll_bar(SkiaScrollBar::new().is_draggable(true).thickness(6))
                .scroll_bar_thumb_color(hex(pal().scroll_thumb))
                .scroll_bar_track_color(Color::new(pal().scroll_track))
                .observe(|me, app: &App| me.set_is_visible(opened(app).is_some()))
                .content(SkiaStack::new().spacing(10).padding((0, 0, 10, 0)).children((
                    label("", 12).text_color(hex(pal().muted)).accessibility_role(Aria::TEXT).observe(|me, app: &App| {
                        let when = opened(app).map(|e| asked(app, e)).unwrap_or_default();
                        me.set_is_visible(!when.is_empty());
                        me.set_text(when);
                    }),
                    label("", 13).text_color(hex(pal().text_soft)).accessibility_role(Aria::TEXT).accessibility_text_selectable(true).observe(|me, app: &App| {
                        if let Some(entry) = opened(app) {
                            me.set_text(entry.question.clone());
                        }
                    }),
                    label("", 14).accessibility_role(Aria::TEXT).accessibility_text_selectable(true).observe(|me, app: &App| {
                        if let Some(entry) = opened(app) {
                            me.set_text(entry.answer.clone());
                        }
                    }),
                ))),
        )))
}

/// Back: from an open answer to the list, from the list out of the screen.
fn back_button() -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(8)
        .padding((8, 5, 12, 5))
        .vertical_options(LayoutOptions::Center)
        .stroke_color(hex(pal().border))
        .stroke_width(1.55)
        .accessibility_role(Aria::BUTTON)
        .accessibility_label("Back")
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_hovered(hover_item)
        .on_tapped(|_me, app: &mut App, cx| go_back(app, cx))
        .children((label("←  Back", 13).text_color(hex(pal().text)),))
}

/// Back: from an open answer to the answers, from a screen to the list.
fn go_back(app: &mut App, cx: &mut Cx) {
    close_popup(app, cx);
    match &mut app.answers {
        Some(view) if view.open.is_some() => view.open = None,
        _ => cx.go_back(app.shell, true),
    }
}

fn close_popup(app: &mut App, cx: &mut Cx) {
    if let Some(popup) = app.popup.take() {
        cx.close_popup_window(popup);
    }
}

/// A toolbar button: small, an icon and a word (from the app's state: Pin turns into Unpin), lighter
/// under the mouse; screen readers read the word.
fn tool_button(look: impl Fn(&App) -> (&'static str, &'static str) + Clone + 'static) -> Build<SkiaShape> {
    let caption = look.clone();
    SkiaShape::new()
        .corner_radius(6)
        .padding((8, 5, 8, 5))
        .accessibility_role(Aria::BUTTON)
        .animation_tapped(SkiaTouchAnimation::Ripple)
        .on_hovered(hover_item)
        .observe(move |me, app: &App| me.set_accessibility_label(look(app).1.to_string()))
        .children((label("", 13).observe(move |me, app: &App| {
            let (icon, word) = caption(app);
            me.set_text(format!("{icon} {word}"));
        }),))
}

/// Pins an entry of the open screen, or unpins it, and puts the list in its new order.
fn toggle_pin(app: &mut App, cx: &mut Cx, index: usize) {
    let Some(view) = &app.answers else { return };
    let Some(Ok(list)) = &view.list else { return };
    let Some(entry) = list.get(index).cloned() else { return };
    let id = view.id.clone();
    let mut st = app.shared.lock().unwrap();
    st.toggle_pin(&id, &entry);
    // The window's copy too, at once: the toolbar and the list change now.
    app.pins = st.pins.clone();
    drop(st);
    fill_answers(app, cx);
}

fn copy_answer(app: &mut App, cx: &mut Cx, index: usize) {
    let text = app.answers.as_ref().and_then(|v| match &v.list {
        Some(Ok(list)) => list.get(index).map(|e| e.answer.clone()),
        _ => None,
    });
    if let Some(text) = text {
        cx.set_clipboard(text);
    }
}

/// An entry's right-click menu: Open, Pin or Unpin, Copy, without opening the answer first.
fn answer_menu_panel() -> Build<SkiaShape> {
    let target = |app: &App| app.answers.as_ref().and_then(|v| v.menu);
    let is_pinned = move |app: &App| {
        let Some(view) = &app.answers else { return false };
        let Some(Ok(list)) = &view.list else { return false };
        target(app).and_then(|i| list.get(i)).is_some_and(|e| pinned(app, e))
    };
    SkiaShape::new()
        .corner_radius(8)
        .background_color(hex(pal().panel))
        .stroke_color(hex(pal().border))
        .stroke_width(1.55)
        .width_request(MENU_WIDTH)
        .padding((4, 4, 4, 4))
        .children(SkiaStack::new().children((
            // Title bar: which question this answer belongs to, one line.
            label("", 12).text_color(hex(pal().text_soft)).max_lines(1).padding((12, 4, 12, 4)).observe(move |me, app: &App| {
                let Some(view) = &app.answers else { return };
                let Some(Ok(list)) = &view.list else { return };
                if let Some(entry) = target(app).and_then(|i| list.get(i)) {
                    me.set_text(entry.question.split_whitespace().collect::<Vec<_>>().join(" "));
                }
            }),
            SkiaShape::new().fill_x().height_request(1).margin((4, 0, 4, 4)).background_color(hex(pal().border)),
            SkiaStack::new().accessibility_role(Aria::MENU).accessibility_label("Answer").children((
                menu_item("📖", "Open", |app, _cx| {
                    if let Some(view) = &mut app.answers {
                        view.open = view.menu.take();
                        view.copied = false;
                    }
                }),
                menu_item("📌", "Pin", |app, cx| {
                    if let Some(index) = app.answers.as_mut().and_then(|v| v.menu.take()) {
                        toggle_pin(app, cx, index);
                    }
                })
                .observe(move |me, app: &App| me.set_is_visible(!is_pinned(app))),
                menu_item("📌", "Unpin", |app, cx| {
                    if let Some(index) = app.answers.as_mut().and_then(|v| v.menu.take()) {
                        toggle_pin(app, cx, index);
                    }
                })
                .observe(move |me, app: &App| me.set_is_visible(is_pinned(app))),
                menu_item("📋", "Copy", |app, cx| {
                    if let Some(index) = app.answers.as_mut().and_then(|v| v.menu.take()) {
                        copy_answer(app, cx, index);
                    }
                }),
            )),
        )))
}

/// The release the banner offers: the newer one, unless the user chose Later for it.
fn offered(app: &App) -> Option<&update::Update> {
    app.update.as_ref().filter(|u| u.version != app.skipped_update)
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
    // Drawn on the CPU: a transparent window on every GPU, so the round corners show the desktop.
    // Small popups that change only on hover: the CPU upload is cheap.
    let options = PopupWindowOptions { close_on_pointer_leave: true, cpu: true, ..Default::default() };
    app.popup = Some(cx.open_popup_window(content, at, options));
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
const SWATCH: f32 = 26.0;
const PALETTE_WIDTH: f32 = 9.0 * SWATCH + 8.0 * 6.0 + 2.0 * 12.0 + 8.0;

/// A session row's background: the active or the resting row, with its tint mixed in, 30 % while
/// active and 10 % at rest, so text stays readable either way.
/// Under the mouse it is a little lighter.
fn row_color(tint: Option<u32>, active: bool, hovered: bool) -> Color {
    let (base, amount) = if active { (pal().row_active, 0.3) } else { (pal().row, 0.1) };
    let row = tint.map_or(base, |t| mix(base, t, amount));
    hex(if hovered { mix(row, pal().hover_toward, pal().hover_row) } else { row })
}

/// `amount` of `to` mixed into `from`, per channel.
fn mix(from: u32, to: u32, amount: f32) -> u32 {
    let channel = |shift: u32| {
        let (a, b) = (((from >> shift) & 0xFF) as f32, ((to >> shift) & 0xFF) as f32);
        (a + (b - a) * amount) as u32
    };
    channel(16) << 16 | channel(8) << 8 | channel(0)
}

/// The tint palette: the session's name, then the colors in one row.
fn palette_panel() -> Build<SkiaShape> {
    SkiaShape::new()
        .corner_radius(8)
        .background_color(hex(pal().panel))
        .stroke_color(hex(pal().border))
        .stroke_width(1.55)
        .width_request(PALETTE_WIDTH)
        .padding((4, 4, 4, 4))
        .children(SkiaStack::new().children((
            label("", 12).text_color(hex(pal().text_soft)).max_lines(1).padding((12, 4, 12, 4)).observe(|me, app: &App| {
                if let Some(m) = &app.menu {
                    me.set_text(m.name.clone());
                }
            }),
            SkiaShape::new().fill_x().height_request(1).margin((4, 0, 4, 4)).background_color(hex(pal().border)),
            // The menu role on the row that holds the colors: Left / Right move between them.
            SkiaRow::new()
                .spacing(6)
                .padding((12, 6, 12, 10))
                .accessibility_role(Aria::MENU)
                .observe(|me, app: &App| {
                    if let Some(m) = &app.menu {
                        me.set_accessibility_label(format!("Tint of {}", m.name));
                    }
                })
                .children(TINTS.iter().map(|&(name, color)| swatch(name, color)).collect::<Vec<_>>()),
        )))
}

/// One color of the palette; a ring marks the session's current tint, a gray one the color under the mouse.
fn swatch(name: &'static str, color: Option<u32>) -> Build<SkiaShape> {
    let ring = move |app: &App, hovered: bool| {
        let current = app.menu.as_ref().is_some_and(|m| app.tint(&m.name) == color);
        if current {
            hex(pal().text)
        } else if hovered {
            hex(pal().text_soft)
        } else {
            Color::TRANSPARENT
        }
    };
    SkiaShape::new()
        .corner_radius(SWATCH / 2.0)
        .width_request(SWATCH)
        .height_request(SWATCH)
        .background_color(color.map_or(hex(pal().row), hex))
        .stroke_width(2)
        .accessibility_role(Aria::MENU_ITEM_RADIO)
        .accessibility_label(name)
        .observe(move |me, app: &App| {
            me.set_stroke_color(ring(app, false));
            me.set_accessibility_is_pressed(Some(app.menu.as_ref().is_some_and(|m| app.tint(&m.name) == color)));
        })
        .on_hovered(move |me, app: &mut App, _cx, on| me.set_stroke_color(ring(app, on)))
        .on_tapped(move |_me, app: &mut App, cx| {
            if let Some(m) = app.menu.take() {
                app.shared.lock().unwrap().set_tint(&m.name, color);
                sync(app, cx);
            }
            menu_done(app, cx);
        })
        // "None" is the plain row with a cross on it.
        .children((label(if color.is_none() { "✕" } else { "" }, 12).text_color(hex(pal().text_soft)).horizontal_options(LayoutOptions::Center).vertical_options(LayoutOptions::Center),))
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
            .background_color(hex(pal().panel))
            .stroke_color(hex(pal().border))
            .stroke_width(1.55)
            .width_request(MENU_WIDTH)
            .padding((4, 4, 4, 4))
            .children(SkiaStack::new().children((
                // Title bar: whose menu this is, one line, so the wrong session is not touched.
                label("", 12)
                    .text_color(hex(pal().text_soft))
                    .max_lines(1)
                    .padding((12, 4, 12, 4))
                    .observe(|me, app: &App| {
                        if let Some(m) = &app.menu {
                            me.set_text(m.name.clone());
                        }
                    }),
                SkiaShape::new().fill_x().height_request(1).margin((4, 0, 4, 4)).background_color(hex(pal().border)),
                // The menu role sits on the column that holds the items: the arrows move between them.
                SkiaStack::new().accessibility_role(Aria::MENU).observe(|me, app: &App| {
                    me.set_is_visible(app.menu.as_ref().is_some_and(|m| m.closed.is_none() && !m.confirm));
                    menu_name(me, app);
                }).children((
                    menu_item("📋", "Copy name", |app, cx| {
                        if let Some(m) = app.menu.take() {
                            cx.set_clipboard(m.name);
                        }
                    }),
                    menu_item("💬", "Answers", open_answers),
                    signal_item(Signal::Say),
                    signal_item(Signal::Sound),
                    signal_item(Signal::Toast),
                    signal_item(Signal::Flash),
                    menu_item("🎨", "Tint", open_palette),
                    // A terminal tab only: a VS Code panel is closed in VS Code.
                    menu_item("❌", "Close session", |app, _cx| {
                        if let Some(m) = &mut app.menu {
                            m.confirm = true;
                        }
                    })
                    .observe(|me, app: &App| {
                        let vscode = app.menu.as_ref().is_some_and(|m| app.sessions.iter().any(|s| s.vscode && s.name == m.name));
                        me.set_is_visible(!vscode);
                    }),
                )),
                // A closed session.
                SkiaStack::new().accessibility_role(Aria::MENU).observe(|me, app: &App| {
                    me.set_is_visible(app.menu.as_ref().is_some_and(|m| m.closed.is_some()));
                    menu_name(me, app);
                }).children((
                    menu_item("📋", "Copy name", |app, cx| {
                        if let Some(m) = app.menu.take() {
                            cx.set_clipboard(m.name);
                        }
                    }),
                    menu_item("💬", "Answers", open_answers),
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
                    .accessibility_role(Aria::ALERT_DIALOG)
                    .accessibility_label("Close session")
                    .children((
                        label("", 13).accessibility_role(Aria::TEXT).observe(|me, app: &App| {
                            if let Some(m) = &app.menu {
                                me.set_text(format!("Close the terminal tab of {}? It stays in the list: click it to reopen.", m.name));
                            }
                        }),
                        SkiaRow::new().spacing(8).children((
                            button("Close tab", pal().danger).width_request(100).on_tapped(|_me, app: &mut App, cx| {
                                if let Some(m) = app.menu.take() {
                                    let _ = app.tx.send(Cmd::CloseTab(m.name));
                                }
                                menu_done(app, cx);
                            }),
                            button("Cancel", pal().border).width_request(100).on_tapped(|_me, app: &mut App, cx| {
                                app.menu = None;
                                menu_done(app, cx);
                            }),
                        )),
                    )),
            )))
}

/// A menu's name for screen readers: whose menu it is.
fn menu_name(me: &mut Mut<'_, SkiaLayout>, app: &App) {
    if let Some(m) = &app.menu {
        me.set_accessibility_label(format!("Menu of {}", m.name));
    }
}

/// A menu toggle: this signal when the session finishes, on or off.
fn signal_item(signal: Signal) -> Build<SkiaShape> {
    let (_, _, name) = signal.labels();
    SkiaShape::new()
        .corner_radius(6)
        .fill_x()
        .padding((12, 9, 12, 9))
        .accessibility_role(Aria::MENU_ITEM_CHECKBOX)
        .accessibility_label(name)
        .observe(move |me, app: &App| {
            if let Some(m) = &app.menu {
                me.set_accessibility_is_pressed(Some(state::finished_signal(&app.rules, &m.name, signal)));
            }
        })
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
    me.set_background_color(if on { hex(pal().border) } else { Color::TRANSPARENT });
}

/// Emoji (status, rule icons, agents' notes) come from the system's emoji font.
fn label(text: &str, size: i32) -> Build<SkiaLabel> {
    SkiaLabel::new(text).font_size(size).text_color(hex(pal().text_body)).system_font_fallback(true)
}

fn section(title: &str) -> Build<SkiaLabel> {
    label(title, 13).text_color(hex(pal().text_soft)).margin((4, 12, 0, 0)).accessibility_role(Aria::HEADING)
}


fn status_color(status: &str) -> Color {
    match status {
        // Yellow: Claude Code waits for you (a permission dialog, an input prompt). Blue: the last
        // answer asked you something. Green works, gray rests (as closed sessions are gray); red is
        // `FINISHED`, an idle session not looked at yet.
        "waiting" => hex(pal().waiting),
        "question" => hex(pal().question),
        "busy" => hex(pal().busy),
        _ => hex(pal().idle),
    }
}

/// One session: status dot, name, note / what it waits for / folder, status and for how long.
/// Left click: select its terminal tab. Right click / Menu key: copy its name, close it.
fn session_row(pid: u32, name: String) -> Build<SkiaShape> {
    let (focus, menu, tint) = (name.clone(), name.clone(), name.clone());
    let key = format!("p{pid}");
    let color = move |app: &App| {
        // Brighter while it needs a look: not idle, or just finished.
        let active = app.session(pid).is_some_and(|s| s.status != "idle" || app.unseen.contains(&s.id));
        row_color(app.tint(&tint), active, app.hovered.as_deref() == Some(key.as_str()))
    };
    let shade = color.clone();
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(pal().row))
        .observe(move |me, app: &App| {
            me.set_background_color(color(app));
            // Read out: the name, what it does now, where it runs.
            let Some(s) = app.session(pid) else { return };
            me.set_is_visible(shows(app, s));
            let status = match s.status.as_str() {
                "waiting" if s.waiting_for.is_empty() => "waiting for you".to_string(),
                "waiting" => format!("waiting for you: {}", s.waiting_for),
                "question" => format!("asks you: {}", s.waiting_for),
                "busy" => "working".into(),
                _ if app.unseen.contains(&s.id) => "finished".into(),
                _ => "idle".into(),
            };
            let place = if s.vscode { ", in VS Code" } else { "" };
            me.set_accessibility_label(format!("{}, {status}{place}", s.name));
            me.set_accessibility_hint(if s.vscode { "Brings its VS Code window to the front" } else { "Brings its terminal tab to the front" }.to_string());
        })
        .on_hovered(move |me, app: &mut App, _cx, on| {
            app.hovered = on.then(|| format!("p{pid}"));
            me.set_background_color(shade(app));
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
                    // A question stays blue until it is answered; else red until the row is clicked.
                    let unseen = app.unseen.contains(&s.id) && s.status != "question";
                    me.set_background_color(if unseen { hex(pal().finished) } else { status_color(&s.status) });
                }),
            SkiaStack::new().margin((22, 0, 84, 0)).spacing(2).children((
                SkiaRow::new().spacing(6).children((
                    label(&name, 15).text_color(hex(pal().text)).vertical_options(LayoutOptions::Center).observe(move |me, app: &App| {
                        let icons = state::rule_icons(&app.rules, &name);
                        me.set_text(if icons.is_empty() { name.clone() } else { format!("{name}  {icons}") });
                    }),
                    // Runs in VS Code, not in a terminal.
                    SkiaShape::new()
                        .corner_radius(4)
                        .background_color(hex(pal().border))
                        .padding((5, 1, 5, 1))
                        .vertical_options(LayoutOptions::Center)
                        .observe(move |me, app: &App| me.set_is_visible(app.session(pid).is_some_and(|s| s.vscode)))
                        .children((label("VSC", 10).text_color(hex(pal().text_body)),)),
                )),
                label("", 12).text_color(hex(pal().text_soft)).max_lines(2).observe(move |me, app: &App| {
                    let Some(s) = app.session(pid) else { return };
                    let note = app.notes.get(&s.name.to_lowercase()).map_or("", |n| n.as_str());
                    let text = if !note.is_empty() {
                        note.to_string()
                    } else if !s.waiting_for.is_empty() {
                        s.waiting_for.clone()
                    } else if !s.title.is_empty() {
                        s.title.clone()
                    } else {
                        s.cwd.clone()
                    };
                    me.set_is_visible(!text.is_empty());
                    me.set_text(text);
                }),
            )),
            SkiaRow::new().spacing(4).horizontal_options(LayoutOptions::End).vertical_options(LayoutOptions::Center).children((
                // A question: a speech bubble with a question mark, in the question's blue.
                SkiaSvg::from_string(icons::MESSAGE_CIRCLE_QUESTION)
                    .width_request(13)
                    .height_request(13)
                    .vertical_options(LayoutOptions::Center)
                    .tint_color(hex(pal().question))
                    .observe(move |me, app: &App| me.set_is_visible(app.session(pid).is_some_and(|s| s.status == "question"))),
                label("", 12).vertical_options(LayoutOptions::Center).observe(move |me, app: &App| {
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
            )),
        ))
}

/// A terminal session that is not running: gray, a click reopens it in a terminal tab.
fn closed_row(id: String, name: String) -> Build<SkiaShape> {
    let (reopen, menu, folder, tint, filter_id) = (id.clone(), id.clone(), id.clone(), name.clone(), id.clone());
    let key = format!("c{id}");
    let color = move |app: &App| row_color(app.tint(&tint), false, app.hovered.as_deref() == Some(key.as_str()));
    let (shade, hover_key) = (color.clone(), format!("c{id}"));
    SkiaShape::new()
        .corner_radius(10)
        .background_color(hex(pal().row))
        .observe(move |me, app: &App| {
            me.set_background_color(color(app));
            me.set_is_visible(app.inactive(&filter_id).is_some_and(|s| shows_closed(app, s)));
        })
        .on_hovered(move |me, app: &mut App, _cx, on| {
            app.hovered = on.then(|| hover_key.clone());
            me.set_background_color(shade(app));
        })
        .padding((12, 10, 12, 10))
        .fill_x()
        .accessibility_role(Aria::BUTTON)
        .accessibility_label(format!("{name}, closed"))
        .accessibility_hint("Reopens it in a terminal")
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
            SkiaShape::new().corner_radius(5).width_request(10).height_request(10).vertical_options(LayoutOptions::Center).background_color(hex(pal().muted)),
            SkiaStack::new().margin((22, 0, 84, 0)).spacing(2).children((
                label(&name, 15).text_color(hex(pal().text_soft)),
                label("", 12).text_color(hex(pal().muted)).max_lines(2).observe(move |me, app: &App| {
                    let cwd = app.inactive(&folder).map_or("", |s| s.cwd.as_str());
                    me.set_is_visible(!cwd.is_empty());
                    me.set_text(cwd.to_string());
                }),
            )),
            label("", 12)
                .text_color(hex(pal().muted))
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
        app.update = st.update.clone();
        app.install = st.install.clone();
        app.theme = st.theme.clone();
        app.language = st.language.clone();
        app.detect_questions = st.detect_questions;
        app.check_updates = st.check_updates;
        app.reopen_home_in = st.reopen_home_in.clone();
        app.pins = st.pins.clone();
        let read = st.answers.clone();
        app.skipped_update = st.skipped_update.clone();
        drop(st);
        let key = (
            app.sessions.iter().map(|s| (s.pid, s.name.clone())).collect::<Vec<_>>(),
            app.inactive.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        );
        if let (Some(view), Some((id, list))) = (&mut app.answers, read) {
            if view.list.is_none() && view.id == id {
                // Pinned answers the conversation no longer holds come from the pins.
                view.list = Some(list.map(|mut list| {
                    for pin in app.pins.iter().filter(|p| p.session == id) {
                        if !list.iter().any(|a| pin.is(&id, a)) {
                            list.push(answers::Answer { question: pin.question.clone(), answer: pin.answer.clone(), at_ms: pin.at_ms });
                        }
                    }
                    list
                }));
                fill_answers(app, cx);
            }
        }
        // The list is in the window once the shell has laid out its root: until then the rows
        // wait, and the next tick tries again (the key stays old).
        if key != app.session_key && cx.find::<SkiaLayout>(app.session_stack).is_some() {
            cx.clear_children(app.session_stack);
            for (pid, name) in &key.0 {
                cx.add_child(app.session_stack, session_row(*pid, name.clone()));
            }
            for s in &app.inactive {
                cx.add_child(app.session_stack, closed_row(s.id.clone(), s.label.clone()));
            }
            app.session_key = key;
        } else if key != app.session_key {
            // Not in the window yet: copy the state again at the next tick.
            app.seen = u64::MAX;
        }
    }
}

/// The window. Every SkiaButton is a button for screen readers and the keyboard (the engine gives
/// it no role of its own).
fn ui(app: App) -> Ui<App> {
    Ui::new(app, build).default_accessibility_role::<SkiaButton>(Aria::BUTTON)
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
    let Some(listener) = http::bind().or_else(http::take_over) else {
        // Already running: wave from the running one instead. An older one that cannot be asked to
        // close, or another program on the port: say so, never quit without a word.
        if http::older_running() {
            platform::error_box("An older version of Agent Signals is still running. Quit it, then open this version again.");
        } else if !http::poke_running() {
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
    update::start(shared.clone());
    let _ = STARTED.set((shared, tx));
    drawnui::run_sized(TITLE, 380.0, 640.0, || {
        let (shared, tx) = STARTED.get().unwrap().clone();
        theme::set(&shared.lock().unwrap().theme);
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
            session_key: (Vec::new(), Vec::new()),
            inactive: Vec::new(),
            tints: HashMap::new(),
            unseen: Default::default(),
            hovered: None,
            shell: Handle::default(),
            route: String::new(),
            filter: String::new(),
            filter_editor: Handle::default(),
            answers_editor: Handle::default(),
            topmost: false,
            theme: String::new(),
            language: String::new(),
            detect_questions: true,
            check_updates: true,
            reopen_home_in: String::new(),
            start_at_login: false,
            settings_note: String::new(),
            pins: Vec::new(),
            answers: None,
            answers_list: Handle::default(),
            update: None,
            skipped_update: String::new(),
            install: update::Install::Idle,
            root: Handle::default(),
            session_stack: Handle::default(),
            menu: None,
            popup: None,
            button: MouseButton::Left,
            size: Size::default(),
        };
        Box::new(
            ui(app)
                .font_bytes("Default", include_bytes!("../assets/OpenSans-Regular.ttf"))
                .background(hex(pal().bg))
                .on_key_down(|app: &mut App, event, cx| {
                    match &*event.key {
                        "Escape" if !app.route.is_empty() => go_back(app, cx),
                        "Comma" if event.keyboard.is_control_pressed() => open_settings(app, cx),
                        "F1" => open_help(app, cx),
                        _ => return false,
                    }
                    true
                })

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

/// The session menu and the tint palette take the keyboard as native menus do. Nothing here picks
/// an item: the items would write the app's real rules and settings files.
#[cfg(test)]
mod menu_keys {
    use super::*;
    use drawnui::testing::Headless;

    fn host(rules: Vec<Rule>) -> Headless<App> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let app = App {
            shared: Arc::new(Mutex::new(State::default())),
            tx,
            seen: 0,
            ticking: true,
            now: 0,
            sessions: Vec::new(),
            rules,
            events: Vec::new(),
            notes: HashMap::new(),
            muted: false,
            reopen: Vec::new(),
            session_key: (Vec::new(), Vec::new()),
            inactive: Vec::new(),
            tints: HashMap::new(),
            unseen: Default::default(),
            hovered: None,
            shell: Handle::default(),
            route: String::new(),
            filter: String::new(),
            filter_editor: Handle::default(),
            answers_editor: Handle::default(),
            topmost: false,
            theme: String::new(),
            language: String::new(),
            detect_questions: true,
            check_updates: true,
            reopen_home_in: String::new(),
            start_at_login: false,
            settings_note: String::new(),
            pins: Vec::new(),
            answers: None,
            answers_list: Handle::default(),
            update: None,
            skipped_update: String::new(),
            install: update::Install::Idle,
            root: Handle::default(),
            session_stack: Handle::default(),
            menu: None,
            popup: None,
            button: MouseButton::Left,
            size: Size::default(),
        };
        let mut host = Headless::new(ui(app), 600, 600, 1.0);
        host.ui.set_keyboard_navigation(true);
        host.ui.set_accessibility_enabled(true);
        host.settle();
        host
    }

    fn focused(host: &Headless<App>) -> String {
        let id = host.ui.accessibility_focused();
        host.ui.accessibility_nodes().iter().find(|n| Some(n.control) == id).map_or(String::new(), |n| n.label.clone())
    }

    fn key(host: &mut Headless<App>, key: &str) -> String {
        host.press_key(key);
        host.settle();
        focused(host)
    }

    fn open(host: &mut Headless<App>) {
        let menu = Menu { name: "api".into(), closed: None, at: Point::new(20.0, 20.0), confirm: false };
        open_menu(&mut host.ui.state, &mut host.ui.tree.cx(), menu);
        host.ui.state_changed();
        host.settle();
    }

    /// The Answers screen lists the entries, a search narrows them, a click shows one in full.
    #[test]
    fn last_answers_panel() {
        let mut host = host(Vec::new());
        let list = vec![
            answers::Answer { question: "where are the logs?".into(), answer: "In C:\\logs, one file a day.".into(), at_ms: 0 },
            answers::Answer { question: "how do I deploy?".into(), answer: "Run dev/deploy.ps1 from the repo.".into(), at_ms: 0 },
        ];
        host.ui.state.answers = Some(AnswersView { id: "a".into(), name: "api".into(), live: true, list: Some(Ok(list)), filter: String::new(), shown: Vec::new(), open: None, menu: None, copied: false });
        // The screen comes up as the menu's Answers brings it: pushed on the shell.
        let shell = host.ui.state.shell;
        host.ui.tree.cx().go_to(shell, "answers", false);
        host.settle();
        fill_answers(&mut host.ui.state, &mut host.ui.tree.cx());
        host.ui.state_changed();
        host.settle();
        let has = |host: &Headless<App>, role: &str, label: &str| host.ui.accessibility_nodes().iter().any(|n| n.role == role && n.label == label);
        let (logs, deploy) = ("In C:\\logs, one file a day. Question: where are the logs?", "Run dev/deploy.ps1 from the repo. Question: how do I deploy?");
        assert!(has(&host, Aria::BUTTON, logs) && has(&host, Aria::BUTTON, deploy));

        // A right click on an entry opens its menu, never the answer.
        let at = host.ui.accessibility_nodes().iter().find(|n| n.label == logs).map(|n| n.rect.center()).unwrap();
        assert!(host.right_click(at.x, at.y));
        let view = host.ui.state.answers.as_ref().unwrap();
        assert_eq!((view.open, view.menu), (None, Some(0)), "menu for the entry, nothing opened");
        assert!(host.ui.state.popup.is_some());

        host.ui.state.answers.as_mut().unwrap().filter = "deploy".into();
        fill_answers(&mut host.ui.state, &mut host.ui.tree.cx());
        host.ui.state_changed();
        host.settle();
        assert!(!has(&host, Aria::BUTTON, logs) && has(&host, Aria::BUTTON, deploy));

        host.ui.state.answers.as_mut().unwrap().open = Some(1);
        host.ui.state_changed();
        host.settle();
        assert!(has(&host, Aria::TEXT, "Run dev/deploy.ps1 from the repo."));
        assert!(has(&host, Aria::BUTTON, "Back") && has(&host, Aria::BUTTON, "Copy") && has(&host, Aria::BUTTON, "Pin") && has(&host, Aria::BUTTON, "Go to session"));

        // A pinned answer comes first and says so; the button turns into Unpin. (Pins set in the
        // window's state only: the Pin button would write the real pins.json.)
        host.ui.state.pins = vec![state::Pin { session: "a".into(), question: "how do I deploy?".into(), answer: "Run dev/deploy.ps1 from the repo.".into(), at_ms: 0 }];
        host.ui.state.answers.as_mut().unwrap().filter = String::new();
        fill_answers(&mut host.ui.state, &mut host.ui.tree.cx());
        host.ui.state_changed();
        host.settle();
        assert_eq!(host.ui.state.answers.as_ref().unwrap().shown, [1, 0], "the pinned one first");
        assert!(has(&host, Aria::BUTTON, "Unpin"));
        host.ui.state.answers.as_mut().unwrap().open = None;
        host.ui.state_changed();
        host.settle();
        assert!(has(&host, Aria::BUTTON, "Pinned. Run dev/deploy.ps1 from the repo. Question: how do I deploy?"));
    }

    /// The filter: every word somewhere in the fields, ignoring case.
    #[test]
    fn filter_words() {
        assert!(matches_filter("", &["api"]));
        assert!(matches_filter("API serv", &["api-server", "C:\\Work"]));
        assert!(matches_filter("work api", &["api-server", "C:\\Work"]));
        assert!(!matches_filter("api zzz", &["api-server", "C:\\Work"]));
    }

    #[test]
    fn menus_work_from_the_keyboard() {
        let mut host = host(Vec::new());
        open(&mut host);
        assert_eq!(focused(&host), "Copy name", "the menu opens on its first item");
        assert_eq!(key(&mut host, "ArrowDown"), "Answers");
        assert_eq!(key(&mut host, "ArrowDown"), "Announce when finished");
        assert_eq!(key(&mut host, "ArrowUp"), "Answers");

        open_palette(&mut host.ui.state, &mut host.ui.tree.cx());
        host.ui.state_changed();
        host.settle();
        assert_eq!(focused(&host), "None", "the palette opens on the current tint");
        assert_eq!(key(&mut host, "ArrowRight"), "Orange");

        // A newer release shows the update banner, read out with its version.
        host.ui.state.update = Some(update::Update { version: "9.9.9".into(), url: "https://example.invalid".into() });
        host.ui.state_changed();
        host.settle();
        let nodes = host.ui.accessibility_nodes();
        assert!(nodes.iter().any(|n| n.label == "Agent Signals 9.9.9 is available"));
        for name in ["Update and restart", "Later"] {
            assert!(nodes.iter().any(|n| n.role == Aria::BUTTON && n.label == name && n.can_interact), "{name} is a button");
        }

        // A signal that is on is not a current choice: the menu still opens on its first item.
        let say: Rule = serde_json::from_str(r#"{"session":"api","on":"finished","say":"{name} finished"}"#).unwrap();
        let mut host = self::host(vec![say]);
        open(&mut host);
        assert_eq!(focused(&host), "Copy name");
    }
}
