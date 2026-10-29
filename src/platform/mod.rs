//! What differs per operating system. The rest of the app (UI, worker, rules, API, the session
//! memory) is shared and calls only what is listed here. Every platform file provides the same
//! items with the same signatures; errors are plain text, shown as they are under Recent.
//!
//! - `Signals` (made once on the worker thread, used only there):
//!   `new()`, `sound(&Path)`, `say(&str)`, `toast(title, text, on_click)` (`on_click` may run on
//!   any thread), `focus_tab(title) -> Result<bool>`, `close_tab(title) -> Result<bool>`
//!   (`Ok(false)`: no terminal tab with that title), `tab_layout() -> Vec<(window, tab, title)>`,
//!   `flash_terminal(title) -> bool` (false: no terminal window with that tab).
//!   A tab title is the session's name after a status marker; match it with `state::tab_is`.
//! - This app's window: `flash_app()`, `raise_app()`, `set_topmost(bool)`, `no_maximize()` (once,
//!   when the window exists: no maximize button, no maximizing by double-click or snapping); `error_box(text)` when
//!   the app cannot start (no window yet).
//! - `focus_editor(cwd) -> Result<bool>`: brings to the front the VS Code window that has the
//!   session's folder open (match its title with `state::editor_title_is`); `Ok(false)`: none.
//! - `pid_alive(pid)`.
//! - Folders: `home_dir()` (where `.claude` lives), `data_dir()` (the per-user app data folder;
//!   the app keeps its files in `AgentSignals` under it), `sound_dirs()` (system sounds),
//!   `SOUND_EXTENSIONS` (first = the one `GET /sounds` lists), `FINISHED_SOUND` (the menu's sound).
//! - Claude Code: `CLAUDE_PROGRAMS` (file names to look for), `claude_dirs()` (where installers put
//!   it when PATH does not have it), `open_session(cwd, command, group)`: a new terminal tab in
//!   `cwd` running `command` (the program, then its arguments) with the `CLAUDE*` variables of
//!   this process removed (`restore::without_claude_markers`); `group` names the terminal window
//!   to collect tabs in, `None` = the window used last.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::*;
