//! `wtm cores`: the processor layout, and where the service runs the
//! processes it placed (`[[pin]]`, `[[separate]]`, `[[reserve_cores]]`, the
//! rescue core). `wtm cores explain <pid>` says why one process has its
//! processors.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Subcommand;
use wtm_core::cpu::Topology;
use wtm_ipc::{Client, PlacementStatus, Request, Response};
use wtm_policy::processor_numbers;

#[derive(Debug, Clone, Subcommand)]
pub enum CoresAction {
    /// Why a process runs on the processors it has
    Explain { pid: u32 },
}

/// The service's plan; `Err` says why it is not known.
fn placement(client: impl FnOnce() -> io::Result<Client>) -> Result<PlacementStatus, String> {
    let mut client = client().map_err(|e| format!("RustWTM service not reachable ({e})"))?;
    match client.call(&Request::Status) {
        Ok(Response::Status(s)) => s
            .placement
            .ok_or_else(|| "the service could not read the processor layout".to_owned()),
        Ok(Response::Error(e)) => Err(e),
        Ok(other) => Err(format!("unexpected response: {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

pub fn run(
    json: bool,
    action: Option<CoresAction>,
    topology: impl FnOnce() -> anyhow::Result<Topology>,
    client: impl FnOnce() -> io::Result<Client>,
    out: &mut impl Write,
) -> anyhow::Result<ExitCode> {
    match action {
        Some(CoresAction::Explain { pid }) => {
            let status = placement(client).map_err(|e| anyhow::anyhow!(e))?;
            writeln!(out, "{}", explain(&status, pid))?;
        }
        None => {
            let topology = topology()?;
            let plan = placement(client);
            if json {
                let value = serde_json::json!({
                    "topology": topology,
                    "placement": plan.as_ref().ok(),
                });
                serde_json::to_writer_pretty(&mut *out, &value)?;
                writeln!(out)?;
            } else {
                layout(&topology, out)?;
                writeln!(out)?;
                match plan {
                    Ok(status) => show(&status, out)?,
                    Err(e) => writeln!(out, "Core placement unknown: {e}.")?,
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn layout(topology: &Topology, out: &mut impl Write) -> io::Result<()> {
    writeln!(out, "GROUP  CPU  CORE  CLASS  TIER     CPU SET  STATE")?;
    for c in &topology.cpus {
        writeln!(
            out,
            "{:>5}  {:>3}  {:>4}  {:>5}  {:<7}  {:>7}  {}",
            c.group,
            c.index,
            c.core,
            c.efficiency_class,
            topology.tier(c.efficiency_class),
            c.id,
            if c.reserved_elsewhere {
                "reserved"
            } else if c.parked {
                "parked"
            } else {
                "available"
            }
        )?;
    }
    writeln!(
        out,
        "Tiers are relative Windows EfficiencyClass values; higher classes are faster. Indices are group-relative."
    )
}

fn show(status: &PlacementStatus, out: &mut impl Write) -> io::Result<()> {
    let numbers = |ids: &[u32]| processor_numbers(&status.topology, ids);
    if status.dry_run {
        writeln!(out, "Dry run: the service writes no placement.")?;
    }
    for s in &status.splits {
        let parts: Vec<String> = s
            .parts
            .iter()
            .map(|(app, cpus)| format!("{app} on CPUs {}", numbers(cpus)))
            .collect();
        writeln!(out, "keep apart '{}': {}", s.entry, parts.join(", "))?;
    }
    for r in &status.reservations {
        writeln!(
            out,
            "reserve cores '{}': CPUs {} kept for its app",
            r.entry,
            numbers(&r.cpus)
        )?;
    }
    for note in &status.notes {
        writeln!(out, "Not met: {note}")?;
    }
    if status.placed.is_empty() {
        writeln!(
            out,
            "No process is placed: every process may use every processor."
        )?;
    } else {
        writeln!(
            out,
            "{:>7}  {:<24}  {:<12}  {:<9}  WHY",
            "PID", "NAME", "CPUS", "HOW"
        )?;
        for p in &status.placed {
            writeln!(
                out,
                "{:>7}  {:<24}  {:<12}  {:<9}  {}",
                p.target.pid,
                p.target.name,
                numbers(&p.cpus),
                if p.strict { "affinity" } else { "CPU Sets" },
                p.why.join(", ")
            )?;
        }
    }
    for (target, why) in &status.left_alone {
        writeln!(out, "{} ({}) left alone: {why}", target.name, target.pid)?;
    }
    Ok(())
}

fn explain(status: &PlacementStatus, pid: u32) -> String {
    if let Some(p) = status.placed.iter().find(|p| p.target.pid == pid) {
        return format!(
            "{} ({pid}) runs on CPUs {} ({}): {}",
            p.target.name,
            processor_numbers(&status.topology, &p.cpus),
            if p.strict {
                "hard affinity"
            } else {
                "CPU Sets"
            },
            p.why.join(", then ")
        );
    }
    if let Some((target, why)) = status.left_alone.iter().find(|(t, _)| t.pid == pid) {
        return format!("{} ({pid}) is left alone: {why}", target.name);
    }
    let dry = if status.dry_run {
        " (the service is in dry run)"
    } else {
        ""
    };
    format!(
        "Process {pid} is not placed{dry}: it may use every processor unless it chose otherwise itself."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtm_core::control::ProcessRef;
    use wtm_core::cpu::LogicalCpu;
    use wtm_ipc::PlacedProcess;

    fn status() -> PlacementStatus {
        let cpus = (0..4u8)
            .map(|i| LogicalCpu {
                id: 256 + u32::from(i),
                group: 0,
                index: i,
                core: i,
                efficiency_class: 0,
                parked: false,
                reserved_elsewhere: false,
            })
            .collect();
        let target = |pid: u32, name: &str| ProcessRef {
            pid,
            create_time: 1,
            name: name.into(),
        };
        PlacementStatus {
            topology: Topology { cpus },
            placed: vec![PlacedProcess {
                target: target(10, "game.exe"),
                cpus: vec![256, 257, 258],
                strict: false,
                why: vec!["keep apart 'Game and stream'".into()],
            }],
            splits: vec![wtm_policy::Split {
                entry: "Game and stream".into(),
                parts: vec![("game.exe".into(), vec![256, 257, 258])],
            }],
            reservations: Vec::new(),
            notes: vec![
                "keep on cores 'Far': cores 40-41 name none of this machine's usable processors"
                    .into(),
            ],
            left_alone: vec![(target(20, "obs64.exe"), "chose its own processors".into())],
            dry_run: false,
        }
    }

    #[test]
    fn explains_a_placed_a_left_alone_and_an_unplaced_process() {
        let s = status();
        assert_eq!(
            explain(&s, 10),
            "game.exe (10) runs on CPUs 0-2 (CPU Sets): keep apart 'Game and stream'"
        );
        assert_eq!(
            explain(&s, 20),
            "obs64.exe (20) is left alone: chose its own processors"
        );
        assert!(explain(&s, 30).starts_with("Process 30 is not placed:"));
    }

    #[test]
    fn shows_splits_notes_and_placed_processes() {
        let mut out = Vec::new();
        show(&status(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("keep apart 'Game and stream': game.exe on CPUs 0-2"),
            "{text}"
        );
        assert!(text.contains("Not met: keep on cores 'Far'"), "{text}");
        let row = text
            .lines()
            .find(|l| l.trim_start().starts_with("10 "))
            .unwrap();
        for part in [
            "game.exe",
            "0-2",
            "CPU Sets",
            "keep apart 'Game and stream'",
        ] {
            assert!(row.contains(part), "{row}");
        }
        assert!(
            text.contains("obs64.exe (20) left alone: chose its own processors"),
            "{text}"
        );
    }

    #[test]
    fn without_the_service_the_layout_still_shows() {
        let mut out = Vec::new();
        let code = run(
            false,
            None,
            || Ok(status().topology),
            || Err(io::Error::other("no pipe")),
            &mut out,
        )
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("GROUP  CPU  CORE"), "{text}");
        assert!(
            text.contains("Core placement unknown: RustWTM service not reachable (no pipe)."),
            "{text}"
        );
    }
}
