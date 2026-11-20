# GUI, CLI and service usage

## GUI

Start **RustWTM** from the Start Menu. Right-click a process for actions.
The badge in the top-right corner shows whether the GUI is connected to the
service; run it as administrator to send commands through the service.

## CLI

```text
wtm list --sort mem -n 20          one-shot listing
wtm list --json                    the same as JSON (also `info`, `ports`)
wtm sensors --json                local read-only hardware readings
wtm cores                         logical processors and relative P/E tiers
wtm affinity 1234 --ecores         use the regular efficient tier
wtm power show                    shared Windows frequency policy (read-only)
wtm top                            live view (c/m/d/p/n to sort, q to quit)
wtm info chrome                    details of every chrome.exe
wtm kill 1234 --tree               end a process and its descendants
wtm suspend notepad / wtm resume notepad
wtm trim chrome                    trim the working sets of chrome.exe processes
wtm dump 1234                      full memory dump file, for debugging
wtm priority 1234 below-normal
wtm affinity 1234 0-3,6
wtm eco 1234 on
wtm limit 1234 --cpu 10 --memory 2G   hard caps (Ctrl+C lifts them)
wtm history --hours 24             long-term per-app usage and findings
wtm app-limit set node.exe --cores 2 --memory 8G   persistent app budget
wtm app-limit list                 configured app limits and their usage
wtm ports --known                  listening ports of dev tools and servers
wtm free-port 3000 --yes           end whatever keeps port 3000 busy
wtm config show                    the service's configuration
wtm config check my.toml           validate a file
wtm config apply my.toml           validate and apply it (elevated)
wtm mcp [--allow-control]          MCP server for AI assistants (stdio)
wtm ai setup --provider anthropic  choose an AI provider (and agree to send facts)
wtm ai key                         store its API key (typed hidden, or piped)
wtm ai models / wtm ai test        list models, check the key
wtm ai explain chrome              what is it, is it safe, should I end it?
wtm ai advise "why is my fan loud?"   advice about the whole PC
wtm ai explain 1234 --dry-run      print what would be sent, send nothing
wtm ai setup --provider anthropic --lookups off   answer only from the facts sent
```

`wtm ai` and the window share `%APPDATA%\RustWTM\ai.toml`. Providers:
`anthropic`, `openai`, `gemini`, `openrouter`, `groq`, `deepseek`, `mistral`,
`xai`, `ollama`, `lmstudio` and `custom` (any OpenAI-compatible `--base-url`).
The key can also come from `RUSTWTM_AI_KEY` or the provider's own variable
(`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …). Requests go from
your session with your key, never from the service; keys travel only over
HTTPS (or to this PC), and redirects are refused so a key never reaches
another host.

To let an MCP client use it, add RustWTM to its server list, e.g. in Claude
Desktop's `claude_desktop_config.json`:

```json
{ "mcpServers": { "rustwtm": { "command": "C:\\Program Files\\RustWTM\\wtm.exe", "args": ["mcp"] } } }
```

or with Claude Code: `claude mcp add rustwtm -- "C:\Program Files\RustWTM\wtm.exe" mcp`.
Tools: `system_overview`, `list_processes`, `process_details`,
`process_report` (usage, path, signature, parent and children, hosted services,
ports, startup entries in one call), `process_tree`, `process_modules`,
`why_not_responding`, `why_wont_it_end`, `network_connections`, `services`,
`startup_items`, `app_history`, `drives`, `recent_events`; prompts
`explain_process`, `diagnose_slowdown` and `security_review`; with
`--allow-control` also `end_process`, `suspend_process`,
`resume_process`, `set_priority` and `efficiency_mode`, which require the
`create_time` that `list_processes` reports, so a PID reused meanwhile is
refused.

Targets are PIDs or image names (`.exe` optional, case-insensitive).

## Service

```text
wtm-service install | uninstall | start | stop
wtm-service cleanup                remove RustWTM's program blocks and firewall rules
wtm-service console --data-dir .\data    run the engine in the foreground
```

The service runs as SYSTEM, so `install` refuses a folder that others than
administrators can change (or that a folder above lets them move): install
from Program Files with the [MSI or reviewed install script](installation.md). To install a development
build from its build folder, set `RUSTWTM_ALLOW_ANY_INSTALL_DIR=1` first, on
a development machine only.
