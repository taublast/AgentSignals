# Agent Signals

If you have dozens of terminals open, each with a Claude Code agent at work, you know the problem:
which one is done, which one is waiting for your answer, and which one is still busy? Agent Signals
puts all of them in one small window and tells you when something happens, so you stop cycling
through tabs to find out.

- **See every session at a glance**: red is waiting for you, yellow is working, green is free.
- **Know when one finishes**: right-click a session and turn on what you want: a spoken
  "api-server finished", a sound, a Windows notification, its terminal flashing on the taskbar.
- **Jump to it**: click a session to bring its terminal tab to the front.
- **Get them back after a reboot**: one click reopens every session that was open, with its
  conversation, in its Windows Terminal window and tab order.

Nothing to configure in Claude Code: the app reads the list of sessions Claude Code already keeps.
A small Windows app written in Rust, its UI drawn with [DrawnUi.Rust](https://github.com/DrawnUi/DrawnUi.Rust).

<img src="docs/screenshot.png" alt="Agent Signals: sessions closed by a reboot offered for reopening, sessions waiting (red), working (yellow) and idle (green) with their signal icons, and recent events" width="420">

## The window

**Top bar**

- **Mute**: silences every signal until you switch it off, for example during a call. Status
  changes are still listed under Recent.
- **On top**: keeps the window above all other windows, so it stays visible while you work in the
  terminals.
- **Reopen at start**: after a reboot, reopens the sessions from last time by itself, without the
  button below.
- **Counts**: how many sessions are waiting for you, working, and idle.

**After a reboot** the list starts with "20 sessions from last time are closed" and their names.
**Reopen 20 sessions from last time** reopens each one with `claude --resume`: its conversation,
its start folder, its permission mode and its name, in Windows Terminal, grouped into windows and
tabs as they were, a few seconds apart. **Not now** forgets them. A reopened session waits for you;
it does not continue interrupted work by itself.

### Sessions started in your home folder

Claude Code never saves trust for the home folder, so a session started there asks "trust this
folder?" every time it opens, also when it is reopened. Start agents in a folder of their own
instead, for example `%USERPROFILE%\Agents`; it asks once and stays trusted. To keep the memory
your agents built in the home folder, give that folder a `.claude\settings.local.json` with

```json
{ "autoMemoryDirectory": "~/.claude/projects/C--Users-<you>/memory" }
```

(the memory folder of your home folder; Claude Code reads this setting only from local or user
settings). Conversations started in the home folder resume from the new folder without trouble.
To have Agent Signals reopen them there too, set `"reopen_home_in": "C:\\Users\\<you>\\Agents"` in
`%APPDATA%\AgentSignals\sessions.json` while the app is closed.

**One row per Claude Code session**, in this order: waiting for you first, then working, then idle
with the one that finished last on top.

- **Click** a row: its Windows Terminal tab comes to the front.
- **Right-click** a row (or the Menu key):
  - **Copy name**: the session's name, to paste into a message to it or to another agent.
  - **🔊 Announce when finished**: the app says "<name> finished" every time it finishes.
  - **🔔 Play a sound when finished**: a short sound (Windows' tada).
  - **💬 Notify when finished**: a Windows notification "<name> finished".
  - **⚡ Flash taskbar when finished**: the session's terminal window flashes on the taskbar until
    you switch to it. Taskbar buttons are usually grouped, so the Windows Terminal button flashes:
    hover it to see which window. Outside Windows Terminal, Agent Signals' own button flashes.
  - **Close session**: closes its terminal tab after you confirm. Claude ends there;
    `claude --resume` brings the conversation back.

  Each signal item turns into "Stop ..." once it is on; choose it again to turn it off.

**Recent**: the last status changes ("frontend finished after 17m") and which signals they gave.

The list scrolls when it is longer than the window; the bar on the right shows where you are.

### What the colors and icons mean

| On a row | Meaning |
|---|---|
| 🔴 red dot | The session waits for you: a question, a permission dialog. |
| 🟡 yellow dot | The session is working. |
| 🟢 dark green dot | The session is idle: it finished and has nothing to do. |
| ✋ 1m | Waiting for you, for 1 minute so far. |
| ⚙ 4m | Working, for 4 minutes so far. |
| 18m (no icon) | Idle, finished 18 minutes ago. |
| 🔊 after the name | You will hear "<name> finished". |
| 🔔 after the name | You will hear a sound when it finishes. |
| 💬 after the name | You will get a Windows notification when it finishes. |
| ⚡ after the name | Its terminal window will flash on the taskbar when it finishes. |
| Line under the name | What it waits for ("input needed"), a status line an agent posted there (see API), or its project folder. |

| In the menu | Meaning |
|---|---|
| 🔇 Stop announcing | Announcing is on for this session; choose it to turn it off. |
| 🔕 Stop the sound | The sound is on; choose it to turn it off. |
| 💬 Stop notifying | Notifications are on; choose it to turn them off. |
| ⚡ Stop flashing | Flashing is on; choose it to turn it off. |

## Under the hood

- **Sessions** come from Claude Code's own registry, `%USERPROFILE%\.claude\sessions\<pid>.json`
  (name, `busy` / `idle` / `waiting`). No hooks, nothing to configure in Claude Code.
- **Events**: `started`, `finished` (busy to idle), `waiting` (needs input, dialog open), `closed`.
- **Rules** say which event of which session gives which signal. The menu's signal items make them;
  they persist in `%APPDATA%\AgentSignals\rules.json`.
- **Optional, for agents**: a local API (below) lets Claude agents add rules (also for `waiting`,
  `started`, `closed`), signal right away, or post a status line under a session.

Windows 10 or 11. Selecting and closing terminal tabs needs Windows Terminal; the list and the
signals work with any terminal.

## Download

[Releases](https://github.com/taublast/AgentSignals/releases) have `agentsignals.exe`: one file, no
installer, run it from any folder. It is not code-signed, so Windows SmartScreen asks once: **More
info**, then **Run anyway**. Prefer to build it yourself? Read on.

## Build and run

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
3. For a release file: `dev\release.ps1` builds `target\dist\release\agentsignals.exe` with local
   folder paths remapped out of the binary, and fails if any is left.

### Start with Windows (optional)

With **Reopen at start** on, this brings your sessions back after every reboot. From the
repository folder:

```powershell
$lnk = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\Agent Signals.lnk")
$lnk.TargetPath = "$PWD\target\release\agentsignals.exe"; $lnk.WorkingDirectory = "$PWD\target\release"; $lnk.Save()
```

## Set up with Claude

Your Claude Code agent can build and start the app for you. Tell it:

```
Set up Agent Signals from https://github.com/taublast/AgentSignals: clone it, follow the README's
Build and run section, start the app, then install skill/agent-signals as my personal skill.
```

It installs the skill by copying `skill/agent-signals` to `%USERPROFILE%\.claude\skills\agent-signals`
and writing the clone's folder in place of `<app-folder>` in its `SKILL.md`. The skill is optional:
it lets agents use the API, for example to add a rule when you ask "tell me when any agent needs me".

## API

Optional, for Claude agents and scripts: `http://127.0.0.1:47801`; `GET /` prints this list.

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

`flash` flashes the session's terminal window (Agent Signals itself when there is no session or
the session is not in Windows Terminal); `raise` brings Agent Signals forward. Text fields take
`{name}` and `{waiting}`. Sounds: a name from `GET /sounds` (`%APPDATA%\AgentSignals\sounds`,
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
- API `raise`: Windows may refuse to bring a window forward from the background; the taskbar button
  flashes instead.

## What it reads and writes

- Reads `%USERPROFILE%\.claude\sessions\*.json` (Claude Code's list of live sessions) and, from
  each session's transcript, its first lines (start folder), its last lines (permission mode) and,
  for a session without a name of its own, its title.
- Writes `%APPDATA%\AgentSignals\rules.json`, `sessions.json` (the open terminal sessions and the
  Reopen at start switch; a session that closes is forgotten 30 s later, so a shutdown keeps them
  all) and, after a crash, `crash.log`; registers the toast AUMID under
  `HKCU\Software\Classes\AppUserModelId\AgentSignals`.
- Starts `wt` (Windows Terminal) only when you reopen sessions.
- Sends nothing anywhere: the API listens on 127.0.0.1 only.

## License

MIT, see [LICENSE](LICENSE). Open Sans (`assets/OpenSans-Regular.ttf`) is under the Apache License 2.0,
see [assets/OpenSans-LICENSE.txt](assets/OpenSans-LICENSE.txt).
