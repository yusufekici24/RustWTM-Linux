# Docker container monitoring

RustWTM 0.24.0 adds a read-only **Containers** page and an Overview summary.
Docker is optional. The process sampler, governor and service keep working when
Docker is missing, busy or unavailable.

## Connection

Start Docker Desktop or a local Docker Engine exposing a Windows named pipe.
RustWTM reads the interactive user's active context. It opens the Engine pipe
with the window's own rights, never through the privileged RustWTM service.
No Docker CLI installation or TCP API exposure is required.

Resolution follows `DOCKER_CONTEXT`, then `DOCKER_HOST`, then `currentContext`
in the user's Docker configuration. An explicit context overrides `DOCKER_HOST`.
`DOCKER_CONFIG` selects the configuration directory; otherwise it is
`%USERPROFILE%\.docker`. Named contexts are resolved through `contexts/meta`.
The default pipe is `npipe:////./pipe/docker_engine`; Docker Desktop's
`desktop-linux` context normally uses `dockerDesktopLinuxEngine`.

Only local `npipe` endpoints are supported. TCP, SSH, Unix-socket and remote
Windows pipe contexts show an explicit unsupported-context message. RustWTM
never changes contexts or falls back silently to a different daemon. The Engine
must support API 1.41 or newer; the client negotiates a compatible version up to
1.53. No credentials are requested, sent or saved by this integration.

## Reading the page

Search by name, image, full/partial container ID or Compose project. Combine
search with the project and running-state filters. Select a container by mouse
or keyboard to open details, including ports, health, creation/start time,
restart count, CPU/memory trends and the last 200 log lines. Logs are selectable
and can be copied. Unsupported logging drivers or oversized log responses show
a read error without hiding the container list.

CPU follows Docker conventions: on Linux one fully used core is 100%, so a
container using several cores can exceed 100%. These values are not added to
Windows system CPU. Linux memory usage subtracts inactive file cache when valid;
the displayed limit is the Engine-reported limit. Windows private-working-set
and CPU fields are used when supplied. Missing metrics are shown as unavailable.

Rates need two counter samples. The first reading, resets, absent counters,
repeated source samples and monitoring gaps do not produce invented zero rates.
Statistics sampling targets the `running` state; other states do not retain
their last live rates. Failed statistics stay
marked until successfully read again. Daemon failures retain the last list with
a stale warning. Samples older than 30 seconds are not presented as current.
Histories contain at most 60 measured values per metric.

## Container alerts

Settings → This window → *Notify when a container stays above* is off by default.
On, it sends a Windows notification when a running container stays at or above
a CPU or memory limit for the chosen time (60 s by default), another when it has
stayed back below for that time (at least 30 s), and one when it stops or is
removed while over a limit. The Containers page lists the last 50 notices,
and Events and the Performance charts' event markers show them too, with
*Docker* as their source; at most three notifications are shown per check, and
the tray menu's *Notifications* switch silences them all. Notices live in the
window only: they are lost when it closes and are not in recorded incidents or
the support bundle. A notice's time is when the reader noticed the change, up
to one reading (10 s, or 2 s while Containers or Overview is open) after it.

The limits are shares, whichever convention Docker reports in: CPU is the share
of all the CPUs a container can use (a Linux container at 200% on 4 cores is at
50%), and the notice also gives Docker's own figure; memory is the share of the
container's limit, or of the host's or Docker VM's memory when it has none,
which is what Docker then reports as the limit.

Containers are followed by name, so a restarted or re-created container (a new
id, as Compose does) keeps its alert. Only fresh measurements count: a
container measured more than 30 seconds ago, Docker being unreachable, a
listing cut at 512 containers or a change of Docker context restarts the wait
and never raises or ends an alert. After an alert ends, the same container and
resource stay quiet for five minutes.

The alerts run in the RustWTM window, also while it waits in the tray, and stop
when it exits; the service does not read Docker.

## Last 24 hours

A container's details show its last 24 hours, a minute at a time: CPU (Docker's
figure, 100% per core for Linux containers) and memory, the average as a line
and the peak behind it. Hovering shows the time, average and peak of a minute.
Only readings RustWTM already makes are recorded: while Containers or Overview
is open, or while container alerts are on (every 10 s). Minutes without a
reading stay empty, never filled in, and the caption says how much of the day
was recorded. Each measurement counts once, however often Docker repeats it.

The history is kept by Docker context and container name, so a restarted or
re-created container goes on with its line. It is saved by the Docker reader,
never the window, to `%LOCALAPPDATA%\RustWTM\container-history.json`: every
five minutes while it changes, at once after *Clear history*, and when the
window closes (minutes since the last save can be lost if it does not close
cleanly). The file holds container names and contexts, minute times and the
CPU and memory figures; nothing else. Minutes older than 24 hours, or dated
after the current minute after a clock change, are dropped; at most 64
containers are kept, the one seen longest ago going first. A file larger than
16 MiB (a full one stays far below), unreadable or written by a newer version
is ignored and replaced. No
file is created until Docker has been read.

## Resource and privacy limits

A single lazy worker polls when Overview or Containers requests data; it becomes
idle after those pages stop requesting data, unless container alerts are on: it
then keeps reading every 10 seconds, without details or logs. Refreshes wait
two seconds after a cycle while a page shows Docker. Each pipe request has a 750 ms overall deadline. Pending overlapped I/O is
cancelled and drained before buffers and handles are released. Pipe clients use
identification-level security, so the server cannot impersonate the window.

Lists are streamed into at most 513 records, showing at most 512 containers.
A cycle attempts at most 16 statistics requests under a two-second budget,
rotating fairly; large installations refresh individual rows less often.
Context metadata scanning is capped at 256 entries, configuration files at
128 KiB, ports at 128 entries per container, list replies at 2 MiB, statistics
at 128 KiB, inspection at 512 KiB and log replies at 256 KiB. Transport limits
include HTTP framing. One container's failed statistics do not fail the list.
A shutdown reply is discarded; an old detail cannot appear under a different
selected container.

All Engine requests are GET: version, container list, statistics, inspection and
logs. The integration does not start, stop, restart, remove, exec into or alter
containers. Container data and logs are held in bounded window memory; they are
not written to the service's history database or saved preferences. Container
alerts put a container's name and its CPU or memory share in a Windows
notification, and the last 24 hours file above holds names and per-minute CPU
and memory figures; logs, ports, images and details are never written.

## Validation

Unit tests use synthetic Engine responses and small private pipes. Timeout and
error paths use injected replies or one gated silent pipe, never resource
exhaustion. GUI tests use offline fixtures and cover filters, keyboard selection,
copy, stale data, late replies, compact layouts and all five languages. GPU
images use fictional containers, not the developer's real workloads.

The opt-in read-only live check needs a local daemon:

```powershell
cargo test -p wtm-core --locked -j 2 docker_live_read_only_sample -- --ignored --test-threads=1 --nocapture
```

It reads the existing daemon and containers; it creates or modifies nothing.
Regular CI does not require Docker Desktop. Live proof is recorded separately
from fixture tests and hosted installer checks in the release review.

Field and stream definitions: [Docker Engine API](https://docs.docker.com/reference/api/engine/version/v1.53/).
