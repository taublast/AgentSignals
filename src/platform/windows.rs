//! Windows: voice and sounds (SAPI), toasts, taskbar flash, window raise / topmost, Windows
//! Terminal tabs (UI Automation; opened with `wt`), process liveness, folders.

use crate::state::tab_is;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotifier};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::Media::Speech::{ISpVoice, SPF_ASYNC, SPF_IS_FILENAME, SPF_IS_NOT_XML, SpVoice};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, TreeScope_Descendants,
    UIA_InvokePatternId, UIA_SelectionItemPatternId, UIA_TabItemControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GetClassNameW, GetWindowTextW,
    GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, IsWindowVisible, MB_ICONERROR, MB_OK, MessageBoxW, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE,
    GWL_STYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOZORDER, SetWindowLongPtrW, WS_MAXIMIZEBOX, SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow,
};
use windows::core::{BOOL, HSTRING, w};

type Result<T> = std::result::Result<T, String>;

const AUMID: &str = "AgentSignals";
const TERMINAL_CLASS: &str = "CASCADIA_HOSTING_WINDOW_CLASS";

pub const SOUND_EXTENSIONS: &[&str] = &["wav"];
pub const FINISHED_SOUND: &str = "tada";
/// Windows Terminal finds a bare `claude` only as an .exe, so npm's `claude.cmd` is started by its
/// full path.
pub const CLAUDE_PROGRAMS: &[&str] = &["claude.exe", "claude.cmd", "claude.bat"];

fn env_dir(key: &str) -> PathBuf {
    PathBuf::from(std::env::var(key).unwrap_or_default())
}

pub fn home_dir() -> PathBuf {
    env_dir("USERPROFILE")
}

pub fn data_dir() -> PathBuf {
    std::env::var("APPDATA").map_or_else(|_| PathBuf::from("."), PathBuf::from)
}

pub fn sound_dirs() -> Vec<PathBuf> {
    vec![PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("Media")]
}

/// The native installer's folder and npm's.
pub fn claude_dirs() -> Vec<PathBuf> {
    vec![home_dir().join(".local").join("bin"), env_dir("APPDATA").join("npm")]
}

/// Windows Terminal's arguments for a new tab in `window` and `cwd` running `command`. `;`
/// separates Windows Terminal commands, so one in a name or folder is escaped.
fn wt_args(cwd: &str, command: &[String], window: &str) -> Vec<String> {
    ["-w", window, "nt", "-d", cwd].iter().map(|a| a.to_string()).chain(command.iter().cloned()).map(|a| a.replace(';', "\\;")).collect()
}

pub fn open_session(cwd: &str, command: &[String], group: Option<&str>) -> Result<()> {
    let mut wt = std::process::Command::new("wt");
    wt.args(wt_args(cwd, command, group.unwrap_or("0")));
    crate::restore::without_claude_markers(&mut wt);
    wt.spawn().map(|_| ()).map_err(|e| format!("Windows Terminal (wt) could not start: {e}"))
}

fn msg(e: windows::core::Error) -> String {
    e.message().to_string()
}

/// The COM objects, made once on the worker thread.
pub struct Signals {
    voice: Option<ISpVoice>,
    notifier: Option<ToastNotifier>,
    toasts: RefCell<Vec<ToastNotification>>,
    uia: Option<IUIAutomation>,
}

impl Signals {
    pub fn new() -> Signals {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let voice = CoCreateInstance(&SpVoice, None, CLSCTX_ALL).inspect_err(|e| eprintln!("SAPI voice: {e}")).ok();
            let uia = CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL).inspect_err(|e| eprintln!("UI Automation: {e}")).ok();
            // An unpackaged app shows toasts under an AUMID registered for the current user.
            let key = w!("Software\\Classes\\AppUserModelId\\AgentSignals");
            let name: Vec<u16> = "Agent Signals\0".encode_utf16().collect();
            let err = RegSetKeyValueW(HKEY_CURRENT_USER, key, w!("DisplayName"), REG_SZ.0, Some(name.as_ptr().cast()), (name.len() * 2) as u32);
            if err.is_err() {
                eprintln!("AUMID registration: {err:?}");
            }
            let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))
                .inspect_err(|e| eprintln!("toast notifier: {e}"))
                .ok();
            Signals { voice, notifier, toasts: RefCell::new(Vec::new()), uia }
        }
    }

    /// Queued on the voice after what it is already saying: signals never overlap.
    pub fn sound(&self, wav: &Path) -> Result<()> {
        let voice = self.voice.as_ref().ok_or("no SAPI voice")?;
        unsafe { voice.Speak(&HSTRING::from(wav), (SPF_ASYNC.0 | SPF_IS_FILENAME.0) as u32, None) }.map_err(msg)
    }

    pub fn say(&self, text: &str) -> Result<()> {
        let voice = self.voice.as_ref().ok_or("no SAPI voice")?;
        unsafe { voice.Speak(&HSTRING::from(text), (SPF_ASYNC.0 | SPF_IS_NOT_XML.0) as u32, None) }.map_err(msg)
    }

    /// Shows a toast; `on_click` runs (on a thread pool thread) when the user clicks it.
    pub fn toast(&self, title: &str, text: &str, on_click: Option<Box<dyn Fn() + Send>>) -> Result<()> {
        let notifier = self.notifier.as_ref().ok_or("no toast notifier")?;
        let xml = format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            escape(title),
            escape(text)
        );
        let doc = XmlDocument::new().map_err(msg)?;
        doc.LoadXml(&HSTRING::from(xml)).map_err(msg)?;
        let toast = ToastNotification::CreateToastNotification(&doc).map_err(msg)?;
        if let Some(on_click) = on_click {
            toast
                .Activated(&TypedEventHandler::new(move |_, _| {
                    on_click();
                    Ok(())
                }))
                .map_err(msg)?;
        }
        notifier.Show(&toast).map_err(msg)?;
        // A click reaches the handler only while the toast object lives, in the popup and in the
        // notification center alike.
        // ponytail: the last 50 toasts stay clickable; older ones in the notification center do nothing.
        let mut shown = self.toasts.borrow_mut();
        if shown.len() == 50 {
            shown.remove(0);
        }
        shown.push(toast);
        Ok(())
    }

    /// Selects the Windows Terminal tab titled after the session and brings its window to the
    /// front. False when no tab has that title.
    pub fn focus_tab(&self, name: &str) -> Result<bool> {
        let Some((hwnd, tab)) = self.find_tab(name).map_err(msg)? else { return Ok(false) };
        unsafe { tab.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId).and_then(|p| p.Select()) }.map_err(msg)?;
        raise(hwnd);
        Ok(true)
    }

    /// Presses the tab's close button: the shell and the Claude session in it end, the tab
    /// goes away. False when no tab has that title.
    pub fn close_tab(&self, name: &str) -> Result<bool> {
        self.close_tab_inner(name).map_err(msg)
    }

    fn close_tab_inner(&self, name: &str) -> windows::core::Result<bool> {
        let Some((_, tab)) = self.find_tab(name)? else { return Ok(false) };
        unsafe {
            let uia = self.uia.as_ref().unwrap();
            let parts = tab.FindAll(TreeScope_Descendants, &uia.CreateTrueCondition()?)?;
            for i in 0..parts.Length()? {
                let part = parts.GetElement(i)?;
                if part.CurrentAutomationId()? == "CloseButton" {
                    part.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?.Invoke()?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Flashes the Windows Terminal window holding the tab titled `name` on the taskbar.
    pub fn flash_terminal(&self, name: &str) -> bool {
        let found = self.find_tab(name).ok().flatten();
        if let Some((hwnd, _)) = &found {
            flash(*hwnd);
        }
        found.is_some()
    }

    /// Every Windows Terminal tab: (window number, tab position, title). Tabs of one window share
    /// the number; numbers follow the windows' order on screen at this moment.
    pub fn tab_layout(&self) -> Vec<(u32, u32, String)> {
        let mut out = Vec::new();
        let Some(uia) = self.uia.as_ref() else { return out };
        unsafe {
            let Ok(any) = uia.CreateTrueCondition() else { return out };
            for (window, hwnd) in windows_of_class(TERMINAL_CLASS).into_iter().enumerate() {
                let Ok(all) = uia.ElementFromHandle(hwnd).and_then(|e| e.FindAll(TreeScope_Descendants, &any)) else { continue };
                let mut tab = 0;
                for i in 0..all.Length().unwrap_or(0) {
                    let Ok(el) = all.GetElement(i) else { continue };
                    if el.CurrentControlType().ok() == Some(UIA_TabItemControlTypeId) {
                        if let Ok(title) = el.CurrentName() {
                            out.push((window as u32, tab, title.to_string()));
                        }
                        tab += 1;
                    }
                }
            }
        }
        out
    }

    /// The Windows Terminal tab whose title, minus its status marker, is `name`, and its window.
    fn find_tab(&self, name: &str) -> windows::core::Result<Option<(HWND, IUIAutomationElement)>> {
        let uia = self.uia.as_ref().ok_or_else(|| windows::core::Error::new(windows::core::HRESULT(-1), "no UI Automation"))?;
        unsafe {
            let any = uia.CreateTrueCondition()?;
            for hwnd in windows_of_class(TERMINAL_CLASS) {
                let tabs = uia.ElementFromHandle(hwnd)?.FindAll(TreeScope_Descendants, &any)?;
                for i in 0..tabs.Length()? {
                    let tab = tabs.GetElement(i)?;
                    if tab.CurrentControlType()? == UIA_TabItemControlTypeId && tab_is(&tab.CurrentName()?.to_string(), name) {
                        return Ok(Some((hwnd, tab)));
                    }
                }
            }
        }
        Ok(None)
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The visible top-level windows of a class.
fn windows_of_class(class: &str) -> Vec<HWND> {
    let mut found = (class.to_string(), Vec::new());
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let (class, found) = unsafe { &mut *(lparam.0 as *mut (String, Vec<HWND>)) };
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
        if String::from_utf16_lossy(&buf[..n]) == *class && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            found.push(hwnd);
        }
        true.into()
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    }
    found.1
}

/// This app's main window (the engine does not hand it out).
/// Found by its title: winit also keeps a visible, unowned helper window ("Winit Thread Event
/// Target") in the process, which "the first visible window" could pick instead.
fn own_window() -> Option<HWND> {
    let mut found: Option<HWND> = None;
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let found = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != unsafe { GetCurrentProcessId() } {
            return true.into();
        }
        let mut buf = [0u16; 64];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
        if String::from_utf16_lossy(&buf[..n]) == crate::TITLE {
            *found = Some(hwnd);
            return false.into();
        }
        true.into()
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    }
    found
}

/// Flashes this app's taskbar button.
pub fn flash_app() {
    if let Some(hwnd) = own_window() {
        flash(hwnd);
    }
}

/// Brings this app's window to the front.
pub fn raise_app() {
    if let Some(hwnd) = own_window() {
        raise(hwnd);
    }
}

/// Flashes the taskbar button until the window comes to the front.
fn flash(hwnd: HWND) {
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    unsafe {
        let _ = FlashWindowEx(&info);
    }
}

/// Restores and activates a window. Windows may refuse to let a background app take the
/// foreground; the taskbar button then flashes instead.
fn raise(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            flash(hwnd);
        }
    }
}

/// A blocking error message, for when the app cannot start.
pub fn error_box(text: &str) {
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(crate::TITLE), MB_OK | MB_ICONERROR);
    }
}

/// Takes the maximize button off this app's window; without it Windows also skips maximizing on a
/// title bar double-click and on a drag to the top of the screen.
pub fn no_maximize() {
    let Some(hwnd) = own_window() else { return };
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        SetWindowLongPtrW(hwnd, GWL_STYLE, style & !(WS_MAXIMIZEBOX.0 as isize));
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE);
    }
}

/// Keeps this app's window above all others, or not.
pub fn set_topmost(on: bool) {
    let Some(hwnd) = own_window() else { return };
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        let _ = SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

pub fn pid_alive(pid: u32) -> bool {
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let alive = GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE;
        let _ = CloseHandle(handle);
        alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn terminal_command() {
        let command: Vec<String> = [r"C:\Users\me\.local\bin\claude.exe", "--resume", "abc", "-n", "api-server"].map(String::from).to_vec();
        assert_eq!(
            wt_args(r"C:\Work", &command, "agents-1").join(" "),
            r"-w agents-1 nt -d C:\Work C:\Users\me\.local\bin\claude.exe --resume abc -n api-server"
        );
        // `;` would start a second Windows Terminal command.
        let odd = wt_args(r"C:\x;y", &["claude".into(), "-n".into(), "a; b".into()], "0");
        assert!(odd.contains(&r"C:\x\;y".to_string()) && odd.contains(&r"a\; b".to_string()));
    }

    /// Opens a real Windows Terminal tab with a stand-in for claude that writes down how it was
    /// started: one tab, the right folder, the name in one piece. Run by hand:
    /// `cargo test --release -- --ignored windows_terminal`.
    #[test]
    #[ignore]
    fn windows_terminal_starts_the_session() {
        let root = std::env::temp_dir().join(format!("agentsignals-test-{}-wt", std::process::id()));
        let (bin, folder, log) = (root.join("fake bin"), root.join("my work; folder"), root.join("log.txt"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        // Like npm's claude.cmd; logs its arguments and folder, then the tab closes by itself.
        std::fs::write(bin.join("claude.cmd"), format!("@echo ARGS=[%*] CWD=[%CD%]>> \"{}\"\r\n", log.display())).unwrap();
        let command: Vec<String> = [bin.join("claude.cmd").to_string_lossy().into_owned(), "--resume".into(), "abc".into(), "-n".into(), "my name; two".into()].to_vec();
        open_session(&folder.to_string_lossy(), &command, Some("agents-signals-test")).unwrap();
        let mut text = String::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(200));
            text = std::fs::read_to_string(&log).unwrap_or_default();
            if !text.is_empty() {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
        let text = std::fs::read_to_string(&log).unwrap_or(text);
        assert_eq!(text.lines().count(), 1, "one tab started: {text}");
        assert!(text.contains("--resume abc -n \"my name; two\""), "{text}");
        assert!(text.contains(&format!("CWD=[{}]", folder.display())), "{text}");
        let _ = std::fs::remove_dir_all(root);
    }
}
