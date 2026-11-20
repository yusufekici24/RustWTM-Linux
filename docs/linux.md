# RustWTM on Linux (Ubuntu 24.04)

RustWTM builds and runs on Linux with the same three programs as on
Windows:

| Program | Windows | Linux |
|---|---|---|
| Window | `rustwtm.exe` | `/usr/bin/rustwtm` (runs as the signed-in user) |
| Command line | `wtm.exe` | `/usr/bin/wtm` |
| Governor | `wtm-service.exe` (Windows service, LocalSystem) | `/usr/bin/wtm-service` (`rustwtm.service`, root) |

The crates above `wtm-sys` are shared; `wtm-sys` has a second back end in
`crates/wtm-sys/src/linux/` with the same public API, so the policy engine,
IPC protocol, GUI and CLI are the same code on both systems.

## Install

```sh
sudo apt install ./rustwtm_<version>_amd64.deb
```

The package installs the programs, `rustwtm.service` (enabled and started),
a desktop entry and the icon. The service keeps its configuration, history
and logs in `/var/lib/rustwtm` (`config.toml`, `history.sqlite`, `logs/`).
New installs start in `dry_run = true`, as on Windows.

### Fleet deployment (ManageEngine Endpoint Central and similar)

`scripts/linux/deploy-rustwtm.sh` installs silently and idempotently as
root. Put the `.deb` (and optionally its `.sha256`, a fleet `config.toml`)
next to the script:

```sh
sudo ./deploy-rustwtm.sh                    # newest rustwtm_*_amd64.deb here
sudo RUSTWTM_SHORTCUT=1 ./deploy-rustwtm.sh # Ctrl+Shift+Esc for every GNOME user
```

| Exit code | Meaning |
|---|---|
| 0 | installed, or already the same version |
| 2 | not run as root |
| 3 | package not found / not a Debian package |
| 4 | checksum mismatch |
| 5 | package installation failed |
| 6 | the service did not come up |
| 7 | the supplied `config.toml` was rejected (`wtm config check`) |

Everything is logged to `/var/log/rustwtm-deploy.log`.
`scripts/linux/remove-rustwtm.sh` removes it (`RUSTWTM_PURGE=1` also deletes
`/var/lib/rustwtm`).

## Build

```sh
sudo apt install build-essential pkg-config libgtk-3-dev libxdo-dev \
    libayatana-appindicator3-dev libssl-dev
cargo build --release -p wtm-gui -p wtm-cli -p wtm-service
packaging/linux/build-deb.sh --no-build      # dist/rustwtm_<ver>_amd64.deb
```

## How each feature maps

| Feature | Windows | Linux |
|---|---|---|
| Process list, CPU / memory / I/O | `NtQuerySystemInformation` | `/proc/PID/{stat,statm,status,io,fd}`; kernel threads are folded into `kthreadd`, as Windows folds kernel work into "System" |
| Process identity across PID reuse | handle + creation time | pidfd + start time; every setter re-checks the start time |
| End / suspend / resume | `TerminateProcess`, `NtSuspendProcess` | `pidfd_send_signal` (SIGKILL, SIGSTOP, SIGCONT) |
| Priority class | `SetPriorityClass` | nice, applied to every thread (Idle 19 … High −10, Realtime −20; never SCHED_FIFO) |
| Efficiency mode (EcoQoS) | power throttling | `SCHED_BATCH` (real-time threads are left alone) |
| I/O priority | `ProcessIoPriority` | `ioprio_set` per thread (idle / best-effort 7, 4, 0 / real-time) |
| Memory priority | `ProcessMemoryPriority` | `oom_score_adj` |
| Trim working set | `SetProcessWorkingSetSizeEx` | `process_madvise(MADV_PAGEOUT)` |
| Affinity, CPU Sets, P/E/LP-E cores | CPU Sets | `sched_setaffinity`; tiers from `/sys/devices/cpu_core`, `cpu_atom` (LP-E = atom without L3, e.g. Meteor Lake) or `cpu_capacity` |
| CPU / memory caps (job objects) | job objects | systemd transient scopes in `rustwtm.slice` (`CPUQuota`, `MemoryMax`, cgroup v2) |
| Block programs from starting | Image File Execution Options | fanotify `FAN_OPEN_EXEC_PERM` (exec is refused; the user gets a notification). Only the answering thread holds the fanotify descriptor, so if it ever stops, the kernel allows everything again |
| Cut a program off the network | Windows Firewall rules | its processes move to `wtmnetblock.slice`; an `inet rustwtm` nftables table (separate from ufw) rejects that cgroup's sockets |
| Per-process network bytes | ETW TCP/IP | `NETLINK_SOCK_DIAG` `tcp_info` (bytes received / acknowledged) per socket, mapped to PIDs through `/proc/PID/fd`. UDP/QUIC have no per-socket counters |
| Per-process, per-file disk I/O | ETW disk + file I/O | `/proc/PID/io` storage bytes, attributed to files by the growth of `fdinfo` positions; the rest is reported without a path |
| GPU per process / engine | GPU performance counters | DRM fdinfo (`drm-engine-*`, `drm-resident-*`): i915, xe, amdgpu, nouveau, msm, panfrost |
| Disks, volumes, SMART | PDH, `IOCTL_STORAGE_*` | `/proc/diskstats`, mountinfo (through LUKS/LVM), sysfs, hwmon temperatures, NVMe health log via `NVME_IOCTL_ADMIN_CMD` |
| Sensors | provider / nvidia-smi | `/sys/class/hwmon` (+ nvidia-smi); fan control stays with the firmware |
| Services | Service Control Manager | systemd (`systemctl`) |
| Sessions / users | WTS | systemd-logind; a process's session is the UID of its user slice |
| Power plans | power schemes | power-profiles-daemon (or ACPI platform profile); the Windows plan GUIDs are kept as identifiers |
| Max processor frequency | power setting per plan | cpufreq `scaling_max_freq` per efficiency class |
| Keep awake | `PowerCreateRequest` | `systemd-inhibit` / `gnome-session-inhibit` |
| IPC | named pipe `\\.\pipe\RustWTM` | Unix socket `/run/rustwtm/rustwtm.sock` (root-owned directory, so the name cannot be squatted); peers by `SO_PEERCRED`; admins are root or members of `sudo`, `admin`, `wheel`, `rustwtm-admins` |
| Secrets (AI key, Telegram) | DPAPI | ChaCha20-Poly1305 with a per-user 0600 key |
| HTTPS | WinHTTP | ureq + rustls with the system trust store (corporate root CAs work) |
| Startup apps | Run keys, Startup folder | XDG autostart (`~/.config/autostart`, `/etc/xdg/autostart`); disabling uses `Hidden=true` |
| Installed apps | Uninstall registry | Debian packages with a desktop entry, snaps, Flatpaks; removal through `pkexec` |
| Package updates | winget / Store | APT, snap, Flatpak |
| Ctrl+Shift+Esc | IFEO `taskmgr.exe` debugger | GNOME custom keybinding (per user; a dconf default for fleets) |
| Updates of RustWTM | MSI from GitHub releases | `rustwtm_<ver>_amd64.deb` from the same releases, installed with `pkexec apt-get` after a hash check |

## Differences and limits

- **Wayland**: other programs' windows are invisible to a task manager, so
  "foreground window", window lists and "bring to front" work for X11 (and
  XWayland) programs only (`wmctrl`, `xprop`).
- **The window never runs as root.** Privileged work goes through the
  service; "run as administrator" options are off on Linux.
- Thread start addresses, handle-level object names beyond file
  descriptors, UAC virtualization and Windows-only mitigation policies do
  not exist on Linux and are reported as unavailable.
- Application limits and network blocks need systemd and cgroup v2 (the
  Ubuntu default). Moving a process into a RustWTM scope takes it out of
  its desktop app scope (GNOME may then not group its window under the app).
- Program blocking matches the executable's file name exactly (Linux names
  are case-sensitive; `.exe` in a configuration is ignored). Core programs
  (shells, `sudo`, `pkexec`, `systemd`, the display stack, RustWTM itself)
  can never be blocked.
- Fan curves are never written on Linux.
