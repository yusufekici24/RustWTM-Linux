# RustWTM security review scope

| Boundary | Source entry points | Review focus |
|---|---|---|
| Local user to LocalSystem | [Service IPC](../crates/wtm-service/src/ipc_server.rs), [client](../crates/wtm-ipc/src/lib.rs), [pipe primitives](../crates/wtm-sys/src/pipe.rs) | Installed-service identity, pipe ACL, client impersonation, admin and session checks, frame limits, shutdown and worker accounting |
| LocalSystem to processes or services | [Engine](../crates/wtm-service/src/engine.rs), [executor](../crates/wtm-service/src/executor.rs), [process controls](../crates/wtm-core/src/control.rs) | PID and creation-time pairing, protected targets, rollback ownership, failure paths |
| Telegram to LocalSystem | [Bot](../crates/wtm-service/src/telegram.rs), [settings](../crates/wtm-policy/src/telegram.rs) | Production build profile, local opt-in, allowlists, private chats, confirmation lifetime, persisted token and outbound diagnostics |
| Device helper to hardware | [Service provider](../crates/wtm-service/src/hardware.rs), [helper](../hardware/Wtm.Hardware/), [hardware setup](hardware.md) | Provider path, capability and staleness checks, lease expiry, restoration and driver compatibility |
| Machine-wide settings and installation | [Task Manager](../crates/wtm-core/src/task_manager.rs), [program blocker](../crates/wtm-service/src/blocker.rs), [firewall](../crates/wtm-sys/src/firewall.rs), [installer](../installer/RustWTM.wxs) | Protected paths, ownership and removal of IFEO/firewall entries, upgrades and repair |
| User-session diagnostics and integrations | [CLI MCP](../crates/wtm-cli/src/mcp.rs), [AI adapters](../crates/wtm-ai/), [Docker monitor](docker.md) | Caller rights, data sent outside the machine, credential handling and service independence |

Record the exact source revision, Cargo features, Windows build, service
account and any installed helper or driver. Machine-wide changes and physical
device writes need an isolated test environment. Use injected failures for
resource-failure paths. See the [repository rules](../CLAUDE.md).
