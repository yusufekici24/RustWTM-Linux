# Core placement: design

Status: implemented for 0.37.0 (phases 0-4 below); not yet released.

## Goal

Decide, in one place, which cores each app runs on:

- **Core rules.** "This app runs on these cores": core numbers
  (`"0-3,8"`), a tier (`"performance"`, `"efficient"`) or a count
  (`2` fastest free cores).
- **Separation groups.** "While two or more of these apps run, they share
  no core." The service splits the cores; when only one is left running it
  gets them all back.
- **Reserved cores.** "While this app runs, these cores are its own":
  everything else is kept off them, as the rescue core keeps everything
  off its core today.
- **Core map.** A page that shows each physical core, what runs on it and
  why, and edits the three kinds of entries.

## What exists

- `wtm-sys::cpu`: `Topology` of logical CPUs with CPU Set id, group,
  index, physical `core` and efficiency class; `CoreSelection` tiers;
  `rescue_split`.
- `wtm-core::control::{confine_cpu_sets, release_cpu_sets}`: process
  default CPU Sets, refusing a process that chose CPU Sets of its own and
  releasing only sets that are still ours. Used by the rescue core alone.
- `set_affinity` / `set_core_affinity`: one-shot hard affinity from the
  Processes menu and the inspector; not recorded, never undone.
- `wtm-policy` rules match one process at a time (`RuleEngine`); the
  service's `Executor` records what each source applied and undoes it
  when a rule changes, a process exits or the service stops. It has no
  affinity state.
- Per-app limits (`[[limit]]`) cap CPU with a job object's rate control,
  not with cores.

## Mechanism

Default: **process default CPU Sets** (`SetProcessDefaultCpuSets`), as the
rescue core uses. A process's threads run on those CPUs unless a thread
or the process chose otherwise; Windows' own reservations
(`reserved_elsewhere`) are respected, and putting a process back is
setting an empty list.

`strict = true` on an entry uses **hard affinity**
(`SetProcessAffinityMask`) instead: binding, but limited to one processor
group (64 logical CPUs), and it overrides an affinity the app set itself.
The original mask is recorded and put back.

Measured on Windows 11 build 26300, 32 logical processors in one group
(a probe setting its own process's CPU Sets and affinity, one `ping`
child, one thread spinning under a second):

| Question | Result |
| --- | --- |
| Does a child inherit its parent's default CPU Sets? | **No**: the child starts with none and may use every processor. |
| Does a child inherit its parent's hard affinity? | **Yes**: the child starts with the parent's mask. |
| Does a running thread follow a changed process default? | **Yes**: every sample after the change ran on the new processor. |
| Thread affinity on A, process default CPU Sets on B | Ran on **A** only: affinity wins. |
| Process affinity on A, process default CPU Sets on B | Ran on **A** only: affinity wins. |

So:

- **Helpers escape a split made by image name.** A browser's or game's
  child processes do not inherit what the plan gave their parent. Every
  entry takes `tree = true` (the default for separation groups and
  reserved-core owners) to count every descendant of a matched process as
  part of the app; the plan places each of them, a newly started one on
  the service's start notification.
- **An affinity set by the app wins.** A process whose affinity is
  narrower than its processor group's (or whose threads set their own) is
  not moved by CPU Sets. The plan reports it as "chose its own cores" and
  leaves it out of a split instead of counting on cores it will not use.
  The rescue core has the same limit today.
- **`strict`** uses affinity, which children inherit: putting back a
  parent also puts back the children that still carry the mask the
  service wrote.

A process that set its own CPU Sets is left alone, and the plan reports
it ("chose its own cores"). One whose sets differ from what the service
last wrote has changed them itself and is let go the same way.

## The plan

A pure function in `wtm-policy` (`placement.rs`), evaluated every sample
and tested without touching a process:

```text
plan(topology, running processes, entries, rescue state) -> {process: cpus, why}
```

1. **Units are physical cores.** A core's SMT siblings always go to the
   same app. Cores Windows reserves elsewhere are left out.
2. **Rescue core first.** While engaged, its core is taken out of every
   placement (today a process with CPU Sets of its own is skipped by the
   rescue core; inside the plan a pinned app moves off it too). When to
   engage stays with `RescueCore`; the CPU Set writes move into the plan,
   so one owner writes CPU Sets. This is the riskiest step: the rescue
   core's existing tests must pass unchanged against the new executor
   before it replaces `rescue.rs`.
3. **Reserved cores** of each running owner come next: the owner gets
   them (or keeps all cores, `owner_only = false`), and every other
   process that the rescue core would confine (`ReserveConfig::confines`:
   not protected, not RustWTM, not exempt) is kept off them. Two owners
   asking for the same core: the earlier entry wins; the later one gets
   what is left and an event says so.
4. **Separation groups.** The running members split the cores left:
   equally, or by `weights`; the first member in the list gets the
   fastest tier first. Every process of a member app shares that app's
   part. A member with a core rule of its own takes the intersection.
5. **Core rules** narrow a process to their cores within what is left.
6. Nothing ever ends up with no core: an entry that cannot be met keeps
   the cores it would have had without it, and the plan says why.
7. **Stable.** Cores are handed out in a fixed order (entry order, then
   core index), never by process id or map order. When a member of a
   group starts or exits, the others keep the cores they have where they
   can. A process counts as running for a group only after it has run for
   `hold_secs` (default 5), and its share is kept as long after it exits,
   so short-lived helpers do not reshuffle the split every sample.

Priority, highest first: rescue core, reserved cores, separation groups,
core rules, manual choices.

The service's `placement.rs` (replacing the CPU Set half of
`rescue.rs`) applies the difference between this plan and what it last
wrote, a bounded number of processes per tick as the rescue core does
now, and puts every process back when its entry is removed, the process
exits from the plan, dry run is turned on or the service stops. Each
change is an event ("game.exe and obs64.exe separated: 6 and 2 cores").

Reserved cores and separation groups can confine many processes for
hours, so a service crash must not leave them confined. Every write is
recorded in `data_dir\placement.json` (process id, create time, CPU Set
ids or original affinity mask) before it is made; on start the service
puts back each recorded process that still runs with exactly what was
written, then clears the record.

## Configuration

```toml
[[pin]]
name = "Compilers on E-cores"
match = { name = "cl.exe" }              # as a rule's match
cores = "efficient"                      # or "0-3,8", or 2

[[separate]]
name = "Game and stream"
apps = ["game.exe", "obs64.exe"]         # image-name wildcards
weights = [3, 1]                         # optional; equal by default
strict = false                           # optional

[[reserve_cores]]
name = "Audio"
app = "reaper.exe"
cores = 2                                # count, list or tier
```

Core rules are their own `[[pin]]` entries, not a rule action: the plan
reads them directly, so the rule engine and the executor never write
CPU Sets, and `Action` stays a small `Copy` type (a core list is not).
A `when` condition can come later. Validation refuses lists naming no
core this machine has, a separation group of fewer than two apps, and
reserved cores that would leave fewer than one core for everything else.

## IPC, CLI and window

- Protocol 19: `Status` gains the plan (`placements`: process, CPUs,
  reason, or why it was left alone) and the topology the service used.
  `Command::Affinity` stays as it is: a manual choice is one-shot and not
  put back. A process the plan places shows its manual choice as "chose
  its own cores" and is left alone. Whether manual choices should become
  recorded and undone is an open question.
- `wtm cores` prints the topology and the current plan; `wtm cores
  explain <pid>` says why a process has its cores.
- A **Cores** page under *Protect*: physical cores as tiles (tier,
  processor numbers, load), outlined in the colour of the reservation or
  app holding them; the plan as a table (processors, CPU Sets or hard
  affinity, the entries that placed it), what cannot be met and what is
  left alone; the entries with an editor (`wtm_policy::set_entry` /
  `remove_entry`, TOML-preserving as Limits' editor is). Before an entry
  is saved the editor runs the plan on the processes running now and says
  which apps would get which processors. The page calls the three kinds
  *Keep on cores*, *Keep apart* and *Reserve cores*; the event log and
  `wtm cores` keep the TOML names (`pin`, `separate`, `reserve_cores`),
  which the window translates. In all 26 languages.

## Phases

0. Hardware checks above: done; they make `tree` necessary.
1. `wtm-policy`: entry types, validation, the plan and its tests. Done.
2. `wtm-service` placement executor with its crash record, the rescue
   core's writes moved into it (its tests unchanged), events and restore
   paths. Done.
3. Protocol 19, status, `wtm cores`. Done.
4. The Cores page, its editor and preview; translations. Done.
5. Review renders in English, Turkish and German; 0.37.0.

Tests prove failure paths through seams (a CPU Set writer passed in), not
by starving the machine.
