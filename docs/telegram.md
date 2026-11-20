# Telegram reporting and control

The bot is off until an administrator saves enabled settings in **Settings →
Telegram**. The standard service build sends reports and alerts. Add only
trusted chat IDs. Group members can read the reports, which can include
process names and image paths, ports, services and events. Process arguments
are excluded from `/info`.

Create a dedicated bot with @BotFather and paste its token in RustWTM. Save
with the bot enabled, message the bot for your chat ID, and add the ID in
RustWTM. Save and send `/help`. The service stores the token in
`%ProgramData%\RustWTM\telegram.toml` with a SYSTEM/Administrators ACL. If
exposed, revoke it in @BotFather and replace it locally.

## Custom builds

| Variant | Cargo command |
|---|---|
| Reports and alerts | `cargo build --locked --release --workspace` |
| No Telegram service code | `cargo build --locked --release --workspace --no-default-features` |
| Optional remote controls | `cargo build --locked --release --workspace --features telegram-control` |

Remote controls require the last build and a separate administrator opt-in
(`control = true`). Use its matching GUI and service. Only allowed private
chats can send control commands or confirm a control action. Groups can still
use reporting and exit notifications. The standard service ignores any
previously saved `control = true` at runtime, and rejects that value over IPC.
Confirmation buttons expire after five minutes and belong to the original
chat and message. The bot rechecks saved settings before handling each update,
including updates delivered by a poll already underway when settings changed.

The custom control build grants the allowed Telegram account access to
privileged process and service actions. Telegram authenticates the chat.
The governor's dry-run setting governs automation. Explicit remote actions
in the control build can take effect while the governor is in dry-run.
Protect the account and its active sessions, and remove its allowlist entry
or disable the bot from RustWTM if the account is compromised.
