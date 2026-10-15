//! Windows side: voice and sounds (SAPI), toasts, taskbar flash, window raise / topmost,
//! Windows Terminal tab focus (UI Automation), process liveness.

use std::path::Path;
use windows::Data::Xml::Dom::XmlDocument;
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
    GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, IsWindowVisible, SW_RESTORE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindow,
};
use windows::core::{BOOL, HSTRING, Result, w};

const AUMID: &str = "AgentSignals";
const TERMINAL_CLASS: &str = "CASCADIA_HOSTING_WINDOW_CLASS";

/// The COM objects, made once on the worker thread.
pub struct Signals {
    voice: Option<ISpVoice>,
    notifier: Option<ToastNotifier>,
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
            Signals { voice, notifier, uia }
        }
    }

    /// Queued on the voice after what it is already saying: signals never overlap.
    pub fn sound(&self, wav: &Path) -> Result<()> {
        let voice = self.voice.as_ref().ok_or_else(no_voice)?;
        unsafe { voice.Speak(&HSTRING::from(wav), (SPF_ASYNC.0 | SPF_IS_FILENAME.0) as u32, None) }
    }

    pub fn say(&self, text: &str) -> Result<()> {
        let voice = self.voice.as_ref().ok_or_else(no_voice)?;
        unsafe { voice.Speak(&HSTRING::from(text), (SPF_ASYNC.0 | SPF_IS_NOT_XML.0) as u32, None) }
    }

    pub fn toast(&self, title: &str, text: &str) -> Result<()> {
        let notifier = self.notifier.as_ref().ok_or_else(|| windows::core::Error::new(windows::core::HRESULT(-1), "no toast notifier"))?;
        let xml = format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            escape(title),
            escape(text)
        );
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        notifier.Show(&ToastNotification::CreateToastNotification(&doc)?)
    }

    /// Selects the Windows Terminal tab titled after the session and brings its window to the
    /// front. False when no tab has that title.
    pub fn focus_tab(&self, name: &str) -> Result<bool> {
        let Some((hwnd, tab)) = self.find_tab(name)? else { return Ok(false) };
        unsafe { tab.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)?.Select()? };
        raise(hwnd);
        Ok(true)
    }

    /// Presses the tab's close button: the shell and the Claude session in it end, the tab
    /// goes away. False when no tab has that title.
    pub fn close_tab(&self, name: &str) -> Result<bool> {
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

    /// The Windows Terminal window holding the tab titled `name`.
    pub fn terminal_window(&self, name: &str) -> Option<HWND> {
        self.find_tab(name).ok().flatten().map(|(hwnd, _)| hwnd)
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
    fn find_tab(&self, name: &str) -> Result<Option<(HWND, IUIAutomationElement)>> {
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

fn no_voice() -> windows::core::Error {
    windows::core::Error::new(windows::core::HRESULT(-1), "no SAPI voice")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A Windows Terminal tab title is `<marker> <session name>` (`✳ idle`, `◐ busy`...).
pub fn tab_is(title: &str, name: &str) -> bool {
    title.trim_start_matches(|c: char| !c.is_alphanumeric()).trim().eq_ignore_ascii_case(name)
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
pub fn own_window() -> Option<HWND> {
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

/// Flashes the taskbar button until the window comes to the front.
pub fn flash(hwnd: HWND) {
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
pub fn raise(hwnd: HWND) {
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() {
            flash(hwnd);
        }
    }
}

pub fn set_topmost(hwnd: HWND, on: bool) {
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
    #[test]
    fn tab_titles() {
        assert!(super::tab_is("✳ api-server", "api-server"));
        assert!(super::tab_is("◑ docs-writer", "docs-writer"));
        assert!(super::tab_is("✳ Fix login redirect loop", "Fix login redirect loop"));
        assert!(!super::tab_is("✳ api-server", "api-serve"));
    }
}
