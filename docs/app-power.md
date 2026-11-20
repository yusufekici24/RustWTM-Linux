# App power: a power plan and keeping awake while an app runs

Process Lasso offers per-app power profiles and a "prevent sleep" option.
RustWTM 0.38 does both with `[[while_running]]` entries ([configuration
reference](configuration.md#while_running--app-power)), and adds two rule
actions that lower a process's I/O and memory priority.

## Measured first

Windows 11 build 26300, 2026-11-19. A PowerShell probe created a power
request (`PowerCreateRequest` with a reason string), set a system-required
and a display-required request on it for 40 seconds, and was run twice at
the same time: as SYSTEM in session 0 through a scheduled task (as the
service runs) and in the signed-in user's session. `powercfg /requests`
was read while both held and after they ended.

| | system required | display required |
|---|---|---|
| Session 0 (SYSTEM) | accepted, listed under `SYSTEM` | refused: `ERROR_NOT_SUPPORTED` (50) |
| User session | accepted | accepted, listed under `DISPLAY` |

Both requests disappeared when the probes ended. So the service keeps the
computer awake, and the RustWTM window (also while hidden in the tray)
keeps the screen on when an entry asks for it; with the window closed the
screen turns off as usual.

The machine's power plans (`powercfg /list`): Balanced, High performance,
Power saver and a custom plan. Machines with Modern Standby often list only
Balanced; an entry naming a plan that is not there is reported, not
applied.

## Design

- **One owner of the plan.** Game mode already switched to High
  performance. Two switchers would race (a game ending would put the old
  plan back while a render still wants High performance), so game mode and
  the entries only ask, and one keeper in the service decides: game mode
  first, then the entries in the order of the file.
- **Crash-safe.** The plan in use before the first switch is written to
  `power-plan.txt` before switching (the file game mode used), and put back
  when no request is left, or at the next start after a crash.
- **The user's choice wins.** Before switching again or back, the keeper
  checks that the active plan is still the one it set. If the user chose
  another, that plan stays and the request is left alone until another
  one comes.
- **Hold.** An entry stays active `hold_secs` (5 s) after its apps exit, so
  an app restarting itself does not switch the plan back and forth.
- **Requests, not `SetThreadExecutionState`.** A power request belongs to a
  handle, carries a reason that `powercfg /requests` shows ("RustWTM:
  'Render'"), and ends with the process, so a crash leaves nothing behind.
- **I/O and memory priority only go down.** They join the executor like
  priority and efficiency mode: the lowest requested wins, the original is
  read when RustWTM first takes the setting, and each source rolls back
  only its own.
- **Tests never touch the machine.** The keeper switches plans and holds
  requests through a `PowerSystem` seam; its tests use a machine in memory.
  The window does not hold a screen request under test. The plan list and a
  request that is only created (never set) are read on the real machine.

IPC moves to protocol 20: the status carries what is switched and kept
awake, and the actions and process details gained fields.
