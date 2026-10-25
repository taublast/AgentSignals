//! macOS: sounds and voice (`afplay` / `say`, one queue so signals never overlap), notifications
//! (`display notification`), Dock icon attention, window raise / floating level, Terminal.app tabs
//! (AppleScript), process liveness, folders.
//!
//! Terminal.app is driven through `osascript`, which needs Automation permission for Terminal
//! (System Settings > Privacy & Security > Automation). Denied, the tab calls return an error
//! saying so rather than looking like "no such tab".

use crate::state::tab_is;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Mutex, OnceLock};

type Result<T> = std::result::Result<T, String>;

/// Field and record separators for the AppleScript tab listing: titles are free text, so the
/// separators are characters a tab title cannot hold.
const FIELD: char = '\u{1}';
const RECORD: char = '\u{2}';

pub const SOUND_EXTENSIONS: &[&str] = &["aiff", "wav"];
pub const FINISHED_SOUND: &str = "Glass";
/// macOS installs it as a plain executable; the native installer and both Homebrew prefixes below.
pub const CLAUDE_PROGRAMS: &[&str] = &["claude"];

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn data_dir() -> PathBuf {
    let home = home_dir();
    if home.as_os_str().is_empty() { PathBuf::from(".") } else { home.join("Library").join("Application Support") }
}

pub fn sound_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from("/System/Library/Sounds"), home_dir().join("Library").join("Sounds")]
}

/// The native installer's folder, then Homebrew on Apple silicon and on Intel.
pub fn claude_dirs() -> Vec<PathBuf> {
    vec![home_dir().join(".local").join("bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]
}

// ---------------------------------------------------------------- AppleScript

/// How long a script is given to answer. The worker thread asks for the tab layout on its way
/// round, so a script that never returns would stop the session list being filled in at all:
/// osascript waits indefinitely for a permission prompt, and a prompt cannot be answered while
/// the screen is locked.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs a script through `osascript`, the script on stdin: `-e` and script files are read as
/// MacRoman on some systems, which mangles the markers in tab titles. Gives up after `PATIENCE`.
fn osascript(script: &str) -> Result<String> {
    let mut osa = Command::new("osascript");
    // No script of ours carries this process's Claude markers into what it starts.
    crate::restore::without_claude_markers(&mut osa);
    let mut child = osa
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("osascript could not start: {e}"))?;
    child.stdin.take().ok_or("osascript has no stdin")?.write_all(script.as_bytes()).map_err(|e| format!("osascript: {e}"))?;
    let deadline = std::time::Instant::now() + PATIENCE;
    let status = loop {
        match child.try_wait().map_err(|e| format!("osascript: {e}"))? {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Terminal did not answer in {}s; a permission prompt may be waiting", PATIENCE.as_secs()));
            }
            None => std::thread::sleep(std::time::Duration::from_millis(25)),
        }
    };
    let read = |pipe: &mut Option<std::process::ChildStdout>| -> String {
        let mut text = String::new();
        if let Some(p) = pipe.as_mut() {
            let _ = std::io::Read::read_to_string(p, &mut text);
        }
        text
    };
    let out = read(&mut child.stdout);
    if status.success() {
        return Ok(out.trim_end().to_string());
    }
    let mut err = String::new();
    if let Some(p) = child.stderr.as_mut() {
        let _ = std::io::Read::read_to_string(p, &mut err);
    }
    Err(script_error(&err))
}

/// An AppleScript failure as something a person can act on. A refused Automation permission is the
/// one worth naming: the message osascript gives for it says only "not allowed".
fn script_error(stderr: &str) -> String {
    let text = stderr.trim().trim_start_matches(|c: char| c.is_ascii_digit() || c == ':').trim();
    let text = text.strip_prefix("execution error:").unwrap_or(text).trim();
    if text.contains("not allowed") || text.contains("-1743") {
        return "Terminal automation not allowed: System Settings > Privacy & Security > Automation".into();
    }
    if text.is_empty() { "osascript failed".into() } else { text.to_string() }
}

/// Text inside an AppleScript string literal.
fn as_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One word of a shell command line.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// One Terminal.app tab.
struct Tab {
    /// Position of its window among Terminal's windows, front to back, right now.
    window: u32,
    /// Terminal's own window id: indexes move when a window is raised, ids do not.
    window_id: i64,
    /// Position of the tab in its window.
    tab: u32,
    title: String,
    tty: String,
}

/// Every Terminal.app tab, front window first. No Terminal running is not an error: no tabs.
fn tabs() -> Result<Vec<Tab>> {
    let script = format!(
        r#"if application "Terminal" is not running then return ""
tell application "Terminal"
    set out to ""
    repeat with i from 1 to count of windows
        set wid to id of window i
        repeat with t from 1 to count of tabs of window i
            try
                set tb to tab t of window i
                try
                    set ct to custom title of tb
                on error
                    set ct to ""
                end try
                try
                    set tt to tty of tb
                on error
                    set tt to ""
                end try
                set out to out & i & "{f}" & wid & "{f}" & t & "{f}" & ct & "{f}" & tt & "{r}"
            end try
        end repeat
    end repeat
    return out
end tell"#,
        f = FIELD,
        r = RECORD
    );
    let out = osascript(&script)?;
    Ok(out
        .split(RECORD)
        .filter(|r| !r.trim().is_empty())
        .filter_map(|record| {
            let f: Vec<&str> = record.split(FIELD).collect();
            match f.as_slice() {
                [w, id, t, title, tty] => Some(Tab {
                    window: w.trim().parse::<u32>().ok()?.saturating_sub(1),
                    window_id: id.trim().parse().ok()?,
                    tab: t.trim().parse::<u32>().ok()?.saturating_sub(1),
                    title: title.to_string(),
                    tty: tty.trim().to_string(),
                }),
                _ => None,
            }
        })
        .collect())
}

/// The tab whose title, minus its status marker, is `name`.
fn find_tab(name: &str) -> Result<Option<Tab>> {
    Ok(tabs()?.into_iter().find(|t| tab_is(&t.title, name)))
}

// ------------------------------------------------------------- opening a tab

/// Terminal windows this app opened, by the group it opened them for, and the one used last.
/// Terminal has no named windows, so the names the shared code gives are kept here.
fn groups() -> &'static Mutex<(Vec<(String, i64)>, Option<i64>)> {
    static GROUPS: OnceLock<Mutex<(Vec<(String, i64)>, Option<i64>)>> = OnceLock::new();
    GROUPS.get_or_init(|| Mutex::new((Vec::new(), None)))
}

/// A new Terminal window in `cwd` running `command`, with this process's `CLAUDE*` variables
/// unset: Terminal starts the shell itself, so the variables are dropped in the command line
/// rather than on the child of this process.
///
/// `group` is remembered against the window that gets opened, but every session opens a window of
/// its own: putting a tab in a *chosen* window needs keystroke injection (Accessibility
/// permission), and aimed at the wrong window that would open a tab in a window of the user's.
pub fn open_session(cwd: &str, command: &[String], group: Option<&str>) -> Result<()> {
    let unset: Vec<String> = crate::restore::claude_markers().iter().map(|k| format!("unset {k};")).collect();
    let line = format!(
        "cd {} && {} exec {}",
        sh_quote(cwd),
        unset.join(" "),
        command.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ")
    );
    // The window the tab landed in, so a later session of the same group can be put beside it.
    // Asking for it must not turn a session that did start into a failure, hence the `try`.
    let script = format!(
        "tell application \"Terminal\"\n    activate\n    set tb to do script {}\n    set wid to 0\n    try\n        set wid to id of (first window whose tabs contains tb)\n    end try\n    return wid\nend tell",
        as_string(&line)
    );
    let opened = osascript(&script)?.trim().parse::<i64>().ok().filter(|id| *id != 0);
    if let (Some(id), Ok(mut g)) = (opened, groups().lock()) {
        if let Some(name) = group {
            g.0.retain(|(n, _)| n != name);
            g.0.push((name.to_string(), id));
        }
        g.1 = Some(id);
    }
    Ok(())
}

// ------------------------------------------------------------------- signals

/// What the signal thread plays, in the order it was asked for.
enum Utterance {
    Sound(PathBuf),
    Say(String),
}

/// The signal queue. One thread plays sounds and speech one after another, so two signals at once
/// are heard one after the other instead of on top of each other, as a single voice does on
/// Windows.
pub struct Signals {
    queue: Sender<Utterance>,
}

impl Signals {
    pub fn new() -> Signals {
        let (tx, rx) = channel::<Utterance>();
        std::thread::spawn(move || {
            while let Ok(item) = rx.recv() {
                let run = match &item {
                    Utterance::Sound(path) => Command::new("afplay").arg(path).status(),
                    // `--` so a text starting with a dash is spoken, not read as an option.
                    Utterance::Say(text) => Command::new("say").arg("--").arg(text).status(),
                };
                match run {
                    Ok(status) if !status.success() => eprintln!("signal: exit {status}"),
                    Err(e) => eprintln!("signal: {e}"),
                    _ => {}
                }
            }
        });
        Signals { queue: tx }
    }

    /// Queued behind what is already playing. A sound that cannot be played is reported here while
    /// it still can be: once queued, only the file's absence is knowable up front.
    pub fn sound(&self, file: &Path) -> Result<()> {
        if !file.is_file() {
            return Err(format!("{} is not a file", file.display()));
        }
        self.queue.send(Utterance::Sound(file.to_path_buf())).map_err(|_| "the signal thread stopped".into())
    }

    pub fn say(&self, text: &str) -> Result<()> {
        self.queue.send(Utterance::Say(text.to_string())).map_err(|_| "the signal thread stopped".into())
    }

    /// A notification in the corner of the screen. A click on it focuses the session's terminal
    /// tab, as a click on its row does.
    ///
    /// Notification Centre only talks to an application bundle, so a bundled Agent Signals posts
    /// through it and hears the click; run straight from `cargo`, there is no bundle and no bundle
    /// identifier to post under, and the notification goes through AppleScript instead, where a
    /// click can only dismiss it.
    pub fn toast(&self, title: &str, text: &str, on_click: Option<Box<dyn Fn() + Send>>) -> Result<()> {
        if bundled() {
            return notify::post(title, text, on_click);
        }
        let script = format!("display notification {} with title {}", as_string(text), as_string(title));
        osascript(&script).map(|_| ())
    }

    /// Selects the Terminal tab titled after the session and brings its window to the front.
    /// False when no tab has that title.
    pub fn focus_tab(&self, name: &str) -> Result<bool> {
        let Some(tab) = find_tab(name)? else { return Ok(false) };
        // A window in the Dock has to be taken out of it first: neither selecting a tab, nor
        // putting the window first, nor activating Terminal brings a minimized window back, and
        // Terminal would come forward showing nothing.
        let script = format!(
            "tell application \"Terminal\"\n    if miniaturized of window id {id} then set miniaturized of window id {id} to false\n    set selected of tab {t} of window id {id} to true\n    set index of window id {id} to 1\n    activate\nend tell",
            t = tab.tab + 1,
            id = tab.window_id
        );
        osascript(&script)?;
        Ok(true)
    }

    /// Ends the session in the tab titled after it, and takes the tab away when it can.
    ///
    /// Terminal's AppleScript has no "close this tab", so the tab's own shell is ended, which ends
    /// the Claude session in it; its window is then closed when that tab was the only one in it. A
    /// tab that shares a window with others stays behind, emptied.
    /// False when no tab has that title.
    pub fn close_tab(&self, name: &str) -> Result<bool> {
        let Some(tab) = find_tab(name)? else { return Ok(false) };
        if tab.tty.is_empty() {
            return Err("the tab does not say which terminal device it uses".into());
        }
        end_shell(&tab.tty)?;
        // Closing the window is only right while that tab is still the one tab in it.
        let script = format!(
            "tell application \"Terminal\"\n    if (count of tabs of window id {id}) is 1 then\n        close window id {id} saving no\n        return \"closed\"\n    end if\n    return \"emptied\"\nend tell",
            id = tab.window_id
        );
        osascript(&script)?;
        Ok(true)
    }

    /// Every Terminal tab: (window number, tab position, title). Tabs of one window share the
    /// number; numbers follow the windows' order on screen at this moment.
    pub fn tab_layout(&self) -> Vec<(u32, u32, String)> {
        tabs().unwrap_or_default().into_iter().map(|t| (t.window, t.tab, t.title)).collect()
    }

    /// Always false: one application cannot make another's Dock icon jump, so the shared code
    /// falls back to this app's own icon, which is what the user sees and what Recent then says.
    pub fn flash_terminal(&self, _name: &str) -> bool {
        false
    }

    /// The title of the tab the user is looking at. Not done on macOS yet: `None` = nothing is
    /// being looked at, so every session that finishes stays highlighted until it is clicked.
    // ponytail: the selected tab of Terminal's front window when Terminal is the frontmost app.
    pub fn viewed_tab(&self) -> Option<String> {
        None
    }
}

/// Ends what runs in a terminal device: its session leader, which hangs up its children with it.
fn end_shell(tty: &str) -> Result<()> {
    let device = tty.trim_start_matches("/dev/");
    let out = Command::new("ps")
        .args(["-t", device, "-o", "pid=,stat="])
        .output()
        .map_err(|e| format!("ps: {e}"))?;
    let listing = String::from_utf8_lossy(&out.stdout);
    // "Ss": a session leader. Nothing left to end is success: the tab is already free.
    let leaders: Vec<i32> = listing
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            parts.next()?.contains('s').then_some(pid)
        })
        .collect();
    for pid in leaders {
        // SIGHUP: what closing a terminal window sends, so shells and Claude Code shut down the
        // way they do by hand.
        unsafe { libc::kill(pid, libc::SIGHUP) };
    }
    Ok(())
}

/// True when the program runs as an application bundle, which Notification Centre needs.
fn bundled() -> bool {
    objc2_foundation::NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// Notification Centre: posting, and the click that comes back.
///
/// Each notification is posted under an identifier of its own, and what to do when it is clicked
/// is kept here against that identifier. The delegate is what Notification Centre calls; it looks
/// the identifier up and runs it. Without a delegate a click only opens the app, which is why the
/// session's tab would not come forward.
mod notify {
    use super::Result;
    use block2::{DynBlock, RcBlock};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{AllocAnyThread, MainThreadMarker, define_class, msg_send};
    use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions, UNNotificationRequest,
        UNNotificationResponse, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };
    
    use std::sync::{Mutex, OnceLock};

    type Click = Box<dyn Fn() + Send>;

    /// What to run when a notification is clicked, by its identifier. Notification Centre keeps
    /// notifications after they are posted, so these are kept too; the oldest go first.
    fn waiting() -> &'static Mutex<Vec<(String, Click)>> {
        static WAITING: OnceLock<Mutex<Vec<(String, Click)>>> = OnceLock::new();
        WAITING.get_or_init(|| Mutex::new(Vec::new()))
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "AgentSignalsNotificationDelegate"]
        #[ivars = ()]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// A click on the notification, in the corner or in Notification Centre.
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, done: &DynBlock<dyn Fn()>) {
                let id = response.notification().request().identifier().to_string();
                let found = waiting().lock().ok().and_then(|mut w| w.iter().position(|(k, _)| *k == id).map(|i| w.remove(i).1));
                if let Some(click) = found {
                    click();
                }
                done.call(());
            }

            /// Show the notification even while Agent Signals is the application in front:
            /// without this, a signal for a session would be silent exactly when the board is
            /// being watched.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(&self, _center: &UNUserNotificationCenter, _notification: &UNNotification, done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
                done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::Sound,));
            }
        }
    );

    /// The delegate, made once and kept for the life of the program.
    fn delegate() -> &'static Mutex<Option<Retained<Delegate>>> {
        static DELEGATE: OnceLock<Mutex<Option<Retained<Delegate>>>> = OnceLock::new();
        DELEGATE.get_or_init(|| Mutex::new(None))
    }

    /// Asks for permission once and takes the clicks. Must run on the main thread.
    fn start(_mtm: MainThreadMarker) {
        let mut held = match delegate().lock() {
            Ok(held) => held,
            Err(_) => return,
        };
        if held.is_some() {
            return;
        }
        let this: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        // The first time, macOS asks the user; after that this is answered from its own records.
        let answered = RcBlock::new(|granted: objc2::runtime::Bool, error: *mut NSError| {
            if !granted.as_bool() {
                let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
                eprintln!("notifications not allowed{}{why}", if why.is_empty() { "" } else { ": " });
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &answered);
        *held = Some(this);
    }

    /// Posts one notification and remembers what its click should do.
    pub fn post(title: &str, text: &str, on_click: Option<Click>) -> Result<()> {
        let id = format!("agentsignals-{}-{}", std::process::id(), crate::state::now_ms());
        if let (Some(click), Ok(mut w)) = (on_click, waiting().lock()) {
            // Notification Centre holds a limited number; so does this.
            if w.len() >= 64 {
                drop(w.remove(0));
            }
            w.push((id.clone(), click));
        }
        let (title, text, id) = (title.to_string(), text.to_string(), id);
        let (tell, heard) = std::sync::mpsc::channel();
        super::on_main(move |mtm| {
            start(mtm);
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&title));
            content.setBody(&NSString::from_str(&text));
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None);
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let tell = tell.clone();
            let posted = RcBlock::new(move |error: *mut NSError| {
                let problem = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
                let _ = tell.send(problem);
            });
            center.addNotificationRequest_withCompletionHandler(&request, Some(&posted));
        });
        // Notification Centre answers on a thread of its own; a short wait turns its refusal into
        // an error the shared code can show, and a slow answer is not treated as a failure.
        match heard.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Some(problem)) => Err(problem),
            _ => Ok(()),
        }
    }
}

// -------------------------------------------------------------- this app's window

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSRequestUserAttentionType, NSWindow, NSWindowLevel};

/// Runs `work` on the main thread, where AppKit has to be used. The worker thread asks for these,
/// and the main thread is inside the engine's event loop, which drains the main queue.
fn on_main(work: impl FnOnce(MainThreadMarker) + Send + 'static) {
    if let Some(mtm) = MainThreadMarker::new() {
        work(mtm);
        return;
    }
    dispatch2::DispatchQueue::main().exec_async(move || {
        // On the main queue by now.
        work(unsafe { MainThreadMarker::new_unchecked() });
    });
}

/// This app's window, by its title: the engine does not hand it out, and the process holds helper
/// windows of winit's besides.
fn own_window(mtm: MainThreadMarker) -> Option<objc2::rc::Retained<NSWindow>> {
    let app = NSApplication::sharedApplication(mtm);
    app.windows().into_iter().find(|w| w.title().to_string() == crate::TITLE)
}

/// Makes this app's Dock icon jump until the user comes to it.
pub fn flash_app() {
    on_main(|mtm| {
        NSApplication::sharedApplication(mtm).requestUserAttention(NSRequestUserAttentionType::CriticalRequest);
    });
}

/// Brings this app's window to the front.
pub fn raise_app() {
    on_main(|mtm| {
        let app = NSApplication::sharedApplication(mtm);
        if let Some(window) = own_window(mtm) {
            if window.isMiniaturized() {
                window.deminiaturize(None);
            }
            window.orderFrontRegardless();
        }
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    });
}

/// No maximizing (the green zoom button). Not done on macOS yet.
// ponytail: disable the window's standardWindowButton(ZoomButton) on the main thread, as set_topmost does.
pub fn no_maximize() {}

/// Keeps this app's window above all others, or not.
pub fn set_topmost(on: bool) {
    on_main(move |mtm| {
        if let Some(window) = own_window(mtm) {
            // 3: the level AppKit gives floating panels; 0: an ordinary window.
            window.setLevel(if on { 3 } else { 0 } as NSWindowLevel);
        }
    });
}

/// A blocking error message, for when the app cannot start. AppleScript shows it: there is no
/// application object yet to hang an alert on.
pub fn error_box(text: &str) {
    let script = format!(
        "display alert {} message {} as critical buttons {{\"OK\"}} default button 1",
        as_string(crate::TITLE),
        as_string(text)
    );
    if osascript(&script).is_err() {
        eprintln!("{}: {text}", crate::TITLE);
    }
}

/// Asking after a process without starting one: this runs for every session twice a second.
/// Signal 0 only checks; a process of another user answers "not allowed", which means it is there.
pub fn pid_alive(pid: u32) -> bool {
    let pid = pid as i32;
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        // A folder or a session name with a quote stays one word of the command line.
        assert_eq!(sh_quote("/Users/me/it's work"), r#"'/Users/me/it'\''s work'"#);
        // And one piece of AppleScript text.
        assert_eq!(as_string(r#"say "hi" \ now"#), r#""say \"hi\" \\ now""#);
    }

    #[test]
    fn tab_listing_is_parsed() {
        // What the AppleScript hands back: 1-based positions, turned into the 0-based ones the
        // shared code counts in, titles kept whole.
        let text = format!("1{FIELD}52852{FIELD}2{FIELD}◑ api-server{FIELD}/dev/ttys003{RECORD}");
        let one: Vec<&str> = text.split(RECORD).filter(|r| !r.trim().is_empty()).collect();
        assert_eq!(one.len(), 1);
        let f: Vec<&str> = one[0].split(FIELD).collect();
        assert_eq!(f[3], "◑ api-server");
        assert!(tab_is(f[3], "api-server"));
    }

    #[test]
    fn this_process_is_alive_and_pid_zero_is_not() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
        // Nothing has this pid: the kernel's maximum is far below it.
        assert!(!pid_alive(900_000_000));
    }

    /// A script that never finishes is given up on instead of holding the worker thread, which
    /// would leave the session list empty. Takes PATIENCE seconds, so it is run by hand.
    #[test]
    #[ignore]
    fn a_script_that_hangs_is_given_up_on() {
        let began = std::time::Instant::now();
        let err = osascript("delay 600").expect_err("a script that never answers must fail");
        assert!(err.contains("did not answer"), "{err}");
        assert!(began.elapsed() >= PATIENCE && began.elapsed() < PATIENCE + std::time::Duration::from_secs(5), "{:?}", began.elapsed());
    }

    #[test]
    fn automation_refusal_is_named() {
        assert_eq!(
            script_error("34:42: execution error: Not authorized to send Apple events to Terminal. (-1743)"),
            "Terminal automation not allowed: System Settings > Privacy & Security > Automation"
        );
        assert_eq!(script_error("66:70: execution error: Terminal got an error: boom (-1728)"), "Terminal got an error: boom (-1728)");
    }

    /// A stand-in for claude that writes down how it was started, then stays alive like a session.
    #[cfg(test)]
    fn fake_claude(root: &Path, log: &Path) -> PathBuf {
        let bin = root.join("fake bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("claude");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho \"ARGS=[$*] CWD=[$PWD] MARK=[${{CLAUDE_TEST_MARKER:-}}]\" >> {}\nsleep 600\n", sh_quote(&log.to_string_lossy())),
        )
        .unwrap();
        Command::new("chmod").arg("+x").arg(&fake).status().unwrap();
        fake
    }

    #[cfg(test)]
    fn wait_for_log(log: &Path) -> String {
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let text = std::fs::read_to_string(log).unwrap_or_default();
            if !text.is_empty() {
                return text;
            }
        }
        String::new()
    }

    /// The tab that appeared since `before`, by the terminal device it was given.
    #[cfg(test)]
    fn new_tab(before: &[String]) -> Option<Tab> {
        tabs().unwrap().into_iter().find(|t| !t.tty.is_empty() && !before.contains(&t.tty))
    }

    /// Opens a real Terminal window with a stand-in for claude, then ends it the way Close session
    /// does and checks the tab is gone. Run by hand, one at a time:
    /// `cargo test --release -- --ignored --test-threads=1`.
    #[test]
    #[ignore]
    fn terminal_starts_and_closes_the_session() {
        let root = std::env::temp_dir().join(format!("agentsignals-test-{}-term", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (folder, log) = (root.join("my work's folder"), root.join("log.txt"));
        std::fs::create_dir_all(&folder).unwrap();
        let fake = fake_claude(&root, &log);
        // A marker of this process, which the new session must not inherit.
        unsafe { std::env::set_var("CLAUDE_TEST_MARKER", "inherited") };

        let name = format!("probe-{}", std::process::id());
        let before: Vec<String> = tabs().unwrap().into_iter().map(|t| t.tty).collect();
        let command: Vec<String> = [fake.to_string_lossy().into_owned(), "--resume".into(), "abc".into(), "-n".into(), name.clone()].to_vec();
        open_session(&folder.to_string_lossy(), &command, Some("agentsignals-test")).unwrap();

        let text = wait_for_log(&log);
        assert_eq!(text.lines().count(), 1, "one window started: {text}");
        assert!(text.contains(&format!("ARGS=[--resume abc -n {name}]")), "{text}");
        assert!(text.contains(&format!("CWD=[{}]", folder.display())), "the session's folder: {text}");
        assert!(text.contains("MARK=[]"), "CLAUDE variables must not reach the session: {text}");

        // Claude Code titles its tab "<marker> <name>"; the stand-in cannot, so it is titled here.
        let tab = new_tab(&before).expect("the new tab");
        let titled = format!("◑ {name}");
        osascript(&format!(
            "tell application \"Terminal\" to set custom title of tab {} of window id {} to {}",
            tab.tab + 1,
            tab.window_id,
            as_string(&titled)
        ))
        .unwrap();

        // Close session: the shell and the session in it end, and the tab goes away with them.
        let signals = Signals::new();
        assert!(signals.close_tab(&name).unwrap(), "the tab was found by its title");
        std::thread::sleep(std::time::Duration::from_millis(1500));
        assert!(new_tab(&before).is_none(), "the tab is gone");
        let left = Command::new("ps").args(["-t", tab.tty.trim_start_matches("/dev/"), "-o", "pid="]).output().unwrap();
        assert!(String::from_utf8_lossy(&left.stdout).trim().is_empty(), "nothing is left running in it");
        // A name nothing is titled after is not an error, just no tab.
        assert!(!signals.close_tab("no-such-session-here").unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    /// Reopens a closed session the way the window's Recent row does, with a stand-in for claude
    /// found on PATH. Run by hand with the test above.
    #[test]
    #[ignore]
    fn a_closed_session_reopens() {
        let root = std::env::temp_dir().join(format!("agentsignals-test-{}-reopen", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (folder, log) = (root.join("reopened here"), root.join("log.txt"));
        std::fs::create_dir_all(&folder).unwrap();
        let fake = fake_claude(&root, &log);
        // Claude Code is looked up on PATH: the stand-in goes first.
        let path = format!("{}:{}", fake.parent().unwrap().display(), std::env::var("PATH").unwrap_or_default());
        unsafe { std::env::set_var("PATH", path) };

        let before: Vec<String> = tabs().unwrap().into_iter().map(|t| t.tty).collect();
        let saved = crate::restore::Saved {
            id: "2f8c1d3e-0000-4444-8888-aaaaaaaaaaaa".into(),
            name: format!("reopened-{}", std::process::id()),
            cwd: folder.to_string_lossy().into_owned(),
            mode: "bypassPermissions".into(),
            ..Default::default()
        };
        crate::restore::reopen_one(saved.clone(), "").unwrap();

        let text = wait_for_log(&log);
        assert!(text.contains(&format!("--resume {}", saved.id)), "the conversation is resumed: {text}");
        assert!(text.contains("--permission-mode bypassPermissions"), "its permission mode comes back: {text}");
        assert!(text.contains(&format!("-n {}", saved.name)), "its name comes back: {text}");
        assert!(text.contains(&format!("CWD=[{}]", folder.display())), "in its own folder: {text}");

        // Leave no window behind.
        if let Some(tab) = new_tab(&before) {
            end_shell(&tab.tty).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(800));
            let _ = osascript(&format!(
                "tell application \"Terminal\"\n    if (count of tabs of window id {id}) is 1 then close window id {id} saving no\nend tell",
                id = tab.window_id
            ));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
