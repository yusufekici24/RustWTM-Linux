# Security policy

RustWTM's service runs as LocalSystem and can change or terminate processes,
so security issues are taken seriously.

## Reporting a vulnerability

Please **do not** open a public issue. Report privately through
[GitHub security advisories](https://github.com/rustwtm/rustwtm/security/advisories/new)
or by email to info@oxog.net. Include steps to reproduce and the
affected version.

## Supported versions

Only the latest release receives fixes.

## Design notes

- Control requests over the named pipe are accepted only from elevated
  administrators. Interactive users can query basic status. Queries with
  additional identity or session requirements are checked individually.
- Process detail queries are executed while impersonating the client.
- The pipe rejects remote clients, is created with
  `FILE_FLAG_FIRST_PIPE_INSTANCE` and always keeps an instance open, and
  clients verify that the server is the installed `RustWTM` service.
- The data directory (`%ProgramData%\RustWTM`, containing `config.toml` and
  the usage history) is writable only by SYSTEM and Administrators.
- Core Windows processes, critical processes and RustWTM itself can never be
  throttled, suspended or terminated.
- Automatic enforcement starts in dry-run mode, and Memory Guard needs a
  separate opt-in. A working-set trim can be observed in Events. Actions
  such as trims and process exits are one-time operations.
- Telegram is disabled until configured by an administrator. Standard builds
  support reporting and alerts. Remote process and service actions require a
  custom service build, a local opt-in and an allowed private chat.
  [Telegram operating modes](docs/telegram.md)
- MCP runs on stdio in the CLI. Control tools require `--allow-control`
  and process creation time, and still use the local caller's rights.
  AI provider requests run in the user's session. [CLI, MCP and AI](docs/usage.md#cli)

## Windows security tools

Task Manager replacement and executable blocking write explicit Image File
Execution Options (IFEO) registrations. Firewall blocking creates named
Windows Firewall rules. Full process dumps are a user-requested diagnostic
and can contain secrets. These operations can be visible to endpoint security
products. [Task Manager integration and restore](docs/task-manager.md)

For a suspected false positive, record the release, artifact SHA-256,
detection name and the user action that preceded it. Keep normal Windows
protections enabled and submit the verified artifact through the security
vendor's reporting channel. [Verify RustWTM](docs/installation.md) ·
[Microsoft file submission](https://www.microsoft.com/en-us/wdsi/filesubmission)

## Service compatibility and review

The GUI and service negotiate an exact IPC protocol before processing
requests. The release notes record each protocol change. Upgrade the GUI,
CLI and service together to acquire behavior changes on both sides. After a
protocol change, a mismatched GUI can run locally while service operations
require a matching installation.

When changing an IPC request, response, or nested type, review its postcard
encoding and enum order. Preserve the existing byte layout when possible and
bump the protocol for incompatible changes. Document the affected upgrade in
the changelog. [Install and upgrade](docs/installation.md)

The [security review scope](docs/security-review-scope.md) identifies the
trust boundaries and verification targets for a source review.
