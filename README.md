# Agent Signals

A small Windows app (Rust, UI drawn with [DrawnUi.Rust](https://github.com/DrawnUi/DrawnUi.Rust)) that
shows every Claude Code session running on this machine and signals when something happens: a
sound, a spoken line, a Windows toast, a taskbar flash. Claude agents set it up through a local
HTTP API ("every time api-server finishes, play a sound").

- **Sessions** come from Claude Code's own registry, `%USERPROFILE%\.claude\sessions\<pid>.json`
  (name, `busy` / `idle` / `waiting`). No hooks, nothing to configure in Claude Code.
- **Events**: `started`, `finished` (busy to idle), `waiting` (needs input, dialog open), `closed`.
- **Rules** fire signals on events; they persist in `%APPDATA%\AgentSignals\rules.json`.
- **Semaphore**: red = waiting for you (a question, a permission dialog), yellow = working, green = idle.
  Order: waiting, working, then idle with the session that finished last on top.
- **Row emoji**: ⚙️ working, ✋ needs you, none when idle; after the name 🔊 speech, 🔔 sound, 💬 toast rules.
- **Click a session** to select its Windows Terminal tab and bring the window forward.
- **Right-click a session** (or Menu key): copy its name, turn "<name> finished" announcements on or
  off, or close its terminal tab after a confirm
  (Claude ends there; `claude --resume` brings the conversation back). Escape closes the menu.
- **Mute** silences everything; **On top** keeps the window above others.

## Run

```
cargo run --release
```

The exe is `target\release\agentsignals.exe` (assets copied next to it). Starting it a second
time flashes the running one instead.

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
