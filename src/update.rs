//! Asks GitHub for the latest release at start and once a day; the window offers its download
//! when it is newer than this build. Nothing is sent but the request itself.

use crate::platform;
use crate::state::Shared;
use std::time::Duration;

const LATEST: &str = "https://api.github.com/repos/taublast/AgentSignals/releases/latest";
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Where "Update and restart" is: nothing asked yet, downloading, or failed with a reason (the
/// banner then offers the download in the browser).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Install {
    #[default]
    Idle,
    Downloading,
    Failed(String),
}

/// A release newer than this build: its version and the download for this system.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    pub url: String,
}

/// Checks on a thread of its own: a slow or missing network never holds up the window or the
/// signals. No answer: nothing shows, the next check is a day later.
pub fn start(shared: Shared) {
    std::thread::spawn(move || {
        // What an update left behind (the replaced program), once the old instance is gone.
        std::thread::sleep(Duration::from_secs(10));
        platform::clean_after_update();
        // A minute's look at the setting: a check turned on runs within a minute, then daily.
        let mut last: Option<std::time::Instant> = None;
        loop {
            let on = shared.lock().unwrap().check_updates;
            if !on {
                last = None;
                let mut st = shared.lock().unwrap();
                if st.update.take().is_some() {
                    st.version += 1;
                }
            } else if last.is_none_or(|t| t.elapsed() >= EVERY) {
                last = Some(std::time::Instant::now());
                let found = platform::http_get(LATEST).ok().and_then(|json| newer(&json, env!("CARGO_PKG_VERSION"), platform::UPDATE_ASSET));
                if let Some(update) = found {
                    let mut st = shared.lock().unwrap();
                    if st.update.as_ref() != Some(&update) {
                        st.update = Some(update);
                        st.version += 1;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    });
}

/// "Update and restart": downloads this system's file, puts it in place of the running app and
/// starts it once this instance has closed. Any failure: the banner says why and offers the browser.
pub fn install(shared: Shared, update: Update) {
    let set = move |state: Install| {
        let mut st = shared.lock().unwrap();
        st.install = state;
        st.version += 1;
    };
    set(Install::Downloading);
    std::thread::spawn(move || {
        let name = update.url.rsplit('/').next().unwrap_or("agentsignals-update");
        let file = std::env::temp_dir().join(format!("{}-{name}", std::process::id()));
        match platform::download(&update.url, &file).and_then(|()| platform::install_update(&file)) {
            // The new version starts as soon as this one has closed.
            Ok(()) => std::process::exit(0),
            Err(why) => set(Install::Failed(why)),
        }
    });
}

/// The release in GitHub's answer when it is newer than `current`, with the download whose name
/// ends in `asset`, else its release page.
fn newer(json: &str, current: &str, asset: &str) -> Option<Update> {
    let release: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = release.get("tag_name")?.as_str()?;
    if version(tag)? <= version(current)? {
        return None;
    }
    let download = release.get("assets").and_then(|a| a.as_array()).and_then(|assets| {
        assets
            .iter()
            .find(|a| a.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.ends_with(asset)))
            .and_then(|a| a.get("browser_download_url")?.as_str())
    });
    let url = download.or_else(|| release.get("html_url")?.as_str())?;
    Some(Update { version: tag.trim_start_matches('v').to_string(), url: url.to_string() })
}

/// "v0.6.5" or "0.6.5" to (0, 6, 5).
pub(crate) fn version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.trim().trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_newer_release_and_its_download() {
        let json = r#"{"tag_name":"v0.7.0","html_url":"https://github.com/x/releases/tag/v0.7.0","assets":[
            {"name":"agentsignals-0.7.0-windows-arm64.exe","browser_download_url":"https://dl/arm64.exe"},
            {"name":"agentsignals-0.7.0-windows-x64.exe","browser_download_url":"https://dl/x64.exe"}]}"#;
        assert_eq!(newer(json, "0.6.5", "-windows-x64.exe"), Some(Update { version: "0.7.0".into(), url: "https://dl/x64.exe".into() }));
        // No download for this system: the release page.
        assert_eq!(newer(json, "0.6.5", "-macos-universal.zip").unwrap().url, "https://github.com/x/releases/tag/v0.7.0");
        // The same or an older release: nothing.
        assert_eq!(newer(json, "0.7.0", "-windows-x64.exe"), None);
        assert_eq!(newer(json, "0.10.0", "-windows-x64.exe"), None);
        // Not GitHub's answer (rate limit, error page): nothing.
        assert_eq!(newer(r#"{"message":"API rate limit exceeded"}"#, "0.6.5", "x"), None);
        assert_eq!(newer("<html>", "0.6.5", "x"), None);
    }
}

/// Asks GitHub for real: the answer parses and has this system's download. Run by hand:
/// `cargo test --release -- --ignored github_answers`.
#[cfg(test)]
#[test]
#[ignore]
fn github_answers() {
    let json = platform::http_get(LATEST).unwrap();
    let update = newer(&json, "0.0.1", platform::UPDATE_ASSET).unwrap();
    assert!(update.url.ends_with(platform::UPDATE_ASSET), "{update:?}");
}
