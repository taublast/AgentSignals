# Agent Signals

If you have dozens of terminals open, each with a Claude Code agent at work, you know the problem:
which one is done, which one is waiting for your answer, and which one is still busy? Agent Signals
puts all of them in one small window and tells you when something happens, so you stop cycling
through tabs to find out.

- **See every session at a glance**: red is waiting for you, yellow is working, green is free.
- **Hear and see it happen**: a spoken "api-server finished", a sound, a Windows notification, a
  flashing taskbar button.
- **Jump to it**: click a session to bring its terminal tab to the front.
- **Just ask your agent**: "say it out loud when build-bot finishes", "tell me when any agent needs
  me". Agents set the app up through its local API.

Nothing to configure in Claude Code: the app reads the list of sessions Claude Code already keeps.
A small Windows app written in Rust, its UI drawn with [DrawnUi.Rust](https://github.com/DrawnUi/DrawnUi.Rust).

<img src="docs/screenshot.png" alt="Agent Signals: sessions waiting (red), working (yellow) and idle (green), with their rule icons and recent events" width="420">

## The window

**Top bar**

- **Mute**: silences every signal (sounds, speech, notifications, flashes) until you switch it off,
  for example during a call. Status changes are still listed under Recent.
- **On top**: keeps the window above all other windows, so it stays visible while you work in the
  terminals.
- **Counts**: how many sessions are waiting for you, working, and idle.

**One row per Claude Code session**, in this order: waiting for you first, then working, then idle
with the one that finished last on top.

- **The dot**: red = it asks you something (a question, a permission dialog), yellow = it is
  working, dark green = it is idle.
- **The name** of the session, then icons for what you get when it changes: 🔊 it speaks,
  🔔 it plays a sound, 💬 it shows a Windows notification.
- **The line under the name**: what it waits for ("input needed"), or a status an agent posted
  there ("running 42 of 120 tests"), or its project folder.
- **On the right**: ✋ waiting or ⚙ working, and for how long. Idle sessions show only the time
  since they finished.
- **Click** a row: its Windows Terminal tab comes to the front.
- **Right-click** a row (or the Menu key):
  - **Copy name**: the session's name, to paste into a message to it or to another agent.
  - **🔊 Announce when finished**: the app says "<name> finished" each time it finishes. Choose it
    again (🔇 Stop announcing) to turn it off.
  - **Close session**: closes its terminal tab after you confirm. Claude ends there; `claude --resume`
    brings the conversation back.

**Recent**: the last status changes ("frontend finished after 17m") and what was signaled.

The list scrolls when it is longer than the window; the bar on the right shows where you are.

## Under the hood

- **Sessions** come from Claude Code's own registry, `%USERPROFILE%\.claude\sessions\<pid>.json`
  (name, `busy` / `idle` / `waiting`). No hooks, nothing to configure in Claude Code.
- **Events**: `started`, `finished` (busy to idle), `waiting` (needs input, dialog open), `closed`.
- **Rules** say which event of which session gives which signal. Agents set them through the API
  below, the menu's announce item sets one too; they persist in `%APPDATA%\AgentSignals\rules.json`.

Windows 10 or 11. Selecting and closing terminal tabs needs Windows Terminal; the list and the
signals work with any terminal.

## Build and run

There is no prebuilt exe: you build it from this source.

1. Check the tools:
   ```powershell
   rustc --version     # 1.85 or newer (built with 1.94)
   ```
   Missing? Install Rust and the Microsoft C++ build tools it links with:
   ```powershell
   winget install Rustlang.Rustup
   winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
2. Build and start it from the repository folder:
   ```powershell
   cargo build --release
   Start-Process .\target\release\agentsignals.exe
   ```
   The first build takes a few minutes: it downloads Skia prebuilt for the drawnui crate. Later
   builds take seconds. Starting it a second time flashes the running one instead.

### Start with Windows (optional)

From the repository folder:

```powershell
$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\Agent Signals.lnk")
$lnk.TargetPath = "$PWD\target\release\agentsignals.exe"; $lnk.WorkingDirectory = "$PWD\target\release"; $lnk.Save()
```

## Set up with Claude

Your Claude Code agent can do all of it. Tell it:

```
Set up Agent Signals from https://github.com/taublast/AgentSignals: clone it, follow the README's
Build and run section, start the app, then install skill/agent-signals as my personal skill.
```

It installs the skill by copying `skill/agent-signals` to `%USERPROFILE%\.claude\skills\agent-signals`
and writing the clone's folder in place of `<app-folder>` in its `SKILL.md`. From then on any agent
knows the app: "say it out loud when api-server finishes", "tell me when any agent needs me".

## API (`http://127.0.0.1:47801`, `GET /` prints this)

| Call | Body | |
|---|---|---|
| `GET /sessions` | | live sessions: name, pid, status, waiting_for, title, since_ms, cwd, note |
| `GET /rules` | | rules |
| `POST /rules` | `{session, on, sound?, say?, toast?, title?, flash?, raise?, once?, min_busy_s?}` | `session` a name or `*`; `on` finished, waiting, started, closed |
| `DELETE /rules/<id>` | | |
| `POST /signal` | `{sound?, say?, toast?, title?, flash?, raise?, session?}` | signal now |
| `POST /note` | `{session, text}` | a line under the session (empty clears) |
| `POST /focus` | `{session}` | select its terminal tab |
| `GET /events` | | last 50 events and what fired |
| `GET /sounds` | | playable .wav names |

Text fields take `{name}` and `{waiting}`. Sounds: a name from `GET /sounds` (`%APPDATA%\AgentSignals\sounds`,
then `C:\Windows\Media`) or a full `.wav` path. Sounds and speech share one queue, so signals never overlap.

```powershell
$r = @{ session = "api-server"; on = "finished"; sound = "tada"; say = "{name} finished" } | ConvertTo-Json
Invoke-RestMethod -Method Post http://127.0.0.1:47801/rules -Body $r -ContentType application/json
```

Localhost only, no authentication: any local process can make it beep.

## Notes

- The session file format is Claude Code's internal one (seen on 2.1.287 to 2.1.294); missing fields are tolerated.
- A session with no name of its own (`dev-3f`) shows its custom or AI title in the terminal tab; the app
  reads that title from the transcript tail to find the tab.
- A toast needs the per-user AUMID `AgentSignals`, registered on start under
  `HKCU\Software\Classes\AppUserModelId\AgentSignals`.
- Windows may refuse to bring a window forward from the background; the taskbar button flashes instead.

## What it reads and writes

- Reads `%USERPROFILE%\.claude\sessions\*.json` (Claude Code's list of live sessions) and, for a
  session without a name of its own, the title lines at the end of its transcript.
- Writes `%APPDATA%\AgentSignals\rules.json` and, after a crash, `crash.log`; registers the toast
  AUMID under `HKCU\Software\Classes\AppUserModelId\AgentSignals`.
- Sends nothing anywhere: the API listens on 127.0.0.1 only.

## License

MIT, see [LICENSE](LICENSE). Open Sans (`assets/OpenSans-Regular.ttf`) is under the Apache License 2.0,
see [assets/OpenSans-LICENSE.txt](assets/OpenSans-LICENSE.txt).
