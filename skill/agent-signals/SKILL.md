---
name: agent-signals
description: Use the Agent Signals desktop app to watch the Claude Code sessions on this machine and signal the user (sound, spoken text, Windows toast, taskbar flash, focus a terminal tab). Trigger on "agent signals", "signals app", "play a sound when <agent> finishes", "say it out loud when", "notify me when", "tell me when <session> is done / needs input", "which agents are busy", "show a status for", "track the agents".
---

# Agent Signals

Windows app in `<app-folder>` with a local HTTP API on `http://127.0.0.1:47801`. It lists every
live Claude Code session on this machine (it reads `~/.claude/sessions` itself) and runs signals.
`GET /` prints the API.

## Start it when it is not running

```powershell
try { Invoke-RestMethod http://127.0.0.1:47801/sessions | Out-Null } catch {
    Start-Process "<app-folder>\target\release\agentsignals.exe"
    Start-Sleep 2
}
```

No exe yet: run `cargo build --release` in `<app-folder>` (see its README).

## Turn the user's request into a rule

The app watches the sessions itself; a rule fires even when no agent is around. Do not poll the
sessions yourself to fire signals: add a rule.

| The user says | Rule |
|---|---|
| "every time api-server finishes, play a sound" | `{session:"api-server", on:"finished", sound:"tada"}` |
| "say it out loud when build-bot finishes" | `{session:"build-bot", on:"finished", say:"{name} finished"}` |
| "tell me when build-bot is done" (one time) | `{session:"build-bot", on:"finished", say:"{name} finished", toast:"{name} finished", once:true}` |
| "warn me when any agent needs me" | `{session:"*", on:"waiting", say:"{name} needs you", flash:true}` |
| "ignore quick answers" | add `min_busy_s: 30` to a `finished` rule |

- `on`: `finished` (busy to idle), `waiting` (asks for input or has a dialog open), `started`, `closed`.
- `session`: the exact name from `GET /sessions` (case does not matter), or `*`. Check the name
  there first.
- Actions, any mix: `sound` (a name from `GET /sounds`, or a full .wav path), `say` (spoken),
  `toast` (+ optional `title`), `flash`, `raise`. Text takes `{name}` and `{waiting}`.
- Rules persist across restarts. List with `GET /rules`, remove with `DELETE /rules/<id>`.
  Tell the user which rule you added (its id) and how it will sound.

```powershell
$r = @{ session = "api-server"; on = "finished"; sound = "tada"; say = "{name} finished" } | ConvertTo-Json
Invoke-RestMethod -Method Post http://127.0.0.1:47801/rules -Body $r -ContentType application/json
```

## Other calls

- Signal now: `POST /signal {say:"build done", sound:"chimes", session:"<your name>"}`.
- A status line under a session: `POST /note {session:"<name>", text:"packing 3 of 7"}`; empty
  `text` clears it.
- `POST /focus {session}` selects that session's Windows Terminal tab. Only when the user asks: it
  changes what they are looking at.
- `GET /events`: the last 50 status changes and what fired. Read it to check a rule worked.

## Manners

- Speech and sounds interrupt the user. Add only what they asked for; prefer `once` for one-off waits.
- Never delete or change rules the user did not ask about.
- Muted (switch in the app): rules still log in `/events`, nothing plays.
