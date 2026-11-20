//! systemd integration: the service runs as `rustwtm.service` (root), with
//! its data in /var/lib/rustwtm. `install` writes the unit when the package
//! did not; `start` / `stop` / `uninstall` drive systemctl.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc;

use anyhow::Context;
use tracing::error;

use crate::blocker::Blocker;
use crate::engine::{self, Paths};
use crate::ipc_server::EngineMsg;
use crate::logging;
use wtm_sys::security::Secured;

pub const SERVICE_NAME: &str = "rustwtm";
const DESCRIPTION: &str =
    "RustWTM Governor: keeps the system responsive by throttling processes that overload the CPU";

/// root: full control; everyone: read (as on Windows, users may read the
/// config but not change what is throttled, ended or blocked).
const DATA_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";

/// Where `install` writes a unit; the package ships /usr/lib/systemd/system.
const ADMIN_UNIT: &str = "/etc/systemd/system/rustwtm.service";
const PACKAGE_UNIT: &str = "/usr/lib/systemd/system/rustwtm.service";

pub fn unit_text(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description={DESCRIPTION}\n\
         Documentation=https://github.com/rustwtm/rustwtm\n\
         After=local-fs.target systemd-logind.service\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} service\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # The governor must keep working when the machine is overloaded.\n\
         Nice=-5\n\
         OOMScoreAdjust=-900\n\
         CPUWeight=1000\n\
         IOWeight=1000\n\
         # Processes moved into job scopes and blocked-network scopes are not\n\
         # the service's: stopping it must not end them.\n\
         KillMode=mixed\n\
         TimeoutStopSec=20\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        exe.display()
    )
}

/// Entry point under systemd: secures the data folder and runs the engine
/// until SIGTERM.
pub fn dispatch() -> anyhow::Result<()> {
    // systemd sets INVOCATION_ID for every unit it starts; anything else is
    // someone running `wtm-service service` by hand.
    anyhow::ensure!(
        std::env::var_os("INVOCATION_ID").is_some(),
        "not started by systemd (rustwtm.service); use `console` to run interactively"
    );
    anyhow::ensure!(
        wtm_sys::shell::is_elevated().unwrap_or(false),
        "the service must run as root"
    );
    let data_dir = engine::default_data_dir();
    let secured = data_dir
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            wtm_sys::security::secure_dir(&data_dir, DATA_DIR_SDDL).map_err(|e| e.to_string())
        });
    let aside = match secured {
        Ok(Secured::Created | Secured::Kept) => None,
        Ok(Secured::Replaced(aside)) => Some(aside),
        Ok(Secured::Contested) | Err(_) => {
            anyhow::bail!("{} could not be made RustWTM's own", data_dir.display())
        }
    };
    let paths = Paths::under(&data_dir);
    let _guard = logging::init(&paths.logs, false);
    if let Some(aside) = aside {
        tracing::warn!(moved_to = %aside.display(), "the data directory was not root's alone and was set aside; a new one was made");
    }
    let (tx, rx) = mpsc::channel();
    let stop = tx.clone();
    ctrlc::set_handler(move || {
        let _ = stop.send(EngineMsg::Stop);
    })
    .context("installing the SIGTERM handler")?;
    let result = engine::run(&paths, tx, &rx);
    if let Err(e) = &result {
        error!(error = %e, "service failed");
    }
    result
}

pub const ALLOW_ANY_INSTALL_DIR: &str = "RUSTWTM_ALLOW_ANY_INSTALL_DIR";

fn check_location(exe: &Path, allow_any: bool) -> anyhow::Result<()> {
    if !allow_any
        && !wtm_sys::security::only_admins_control(exe)
            .with_context(|| format!("checking who may change {}", exe.display()))?
    {
        anyhow::bail!(
            "{} can be changed by others than root; the service runs as root, so install it under /usr \
             (the .deb package does), or set {ALLOW_ANY_INSTALL_DIR}=1 on a development machine",
            exe.display()
        );
    }
    Ok(())
}

pub const INSTALL_DIR_SDDL: &str = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";

pub fn prepare_install_dir(dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        wtm_sys::security::only_admins_move(dir)
            .with_context(|| format!("checking the folders above {}", dir.display()))?,
        "{} is not a plain absolute path, or a folder above it can be moved by others than root",
        dir.display()
    );
    match wtm_sys::security::secure_program_dir(dir, INSTALL_DIR_SDDL)
        .with_context(|| format!("securing {}", dir.display()))?
    {
        Secured::Contested => anyhow::bail!("{} kept changing while it was secured", dir.display()),
        Secured::Replaced(aside) => println!(
            "{} held what was not root's; moved to {}",
            dir.display(),
            aside.display()
        ),
        Secured::Created | Secured::Kept => {}
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .context("running systemctl")?;
    anyhow::ensure!(status.success(), "systemctl {} failed", args.join(" "));
    Ok(())
}

pub fn install() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let allow_any = std::env::var_os(ALLOW_ANY_INSTALL_DIR).is_some_and(|v| v == "1");
    check_location(&exe, allow_any)?;
    if !Path::new(PACKAGE_UNIT).exists() {
        std::fs::write(ADMIN_UNIT, unit_text(&exe))
            .with_context(|| format!("writing {ADMIN_UNIT} (run as root)"))?;
        systemctl(&["daemon-reload"])?;
    }
    systemctl(&["enable", "rustwtm.service"])?;
    println!("Installed service '{SERVICE_NAME}'. Start it with `wtm-service start`.");
    println!(
        "Config: {}",
        Paths::under(&engine::default_data_dir()).config.display()
    );
    Ok(())
}

pub fn start() -> anyhow::Result<()> {
    systemctl(&["start", "rustwtm.service"])?;
    println!("Service started.");
    Ok(())
}

pub fn stop() -> anyhow::Result<()> {
    systemctl(&["stop", "rustwtm.service"])?;
    println!("Service stopped.");
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let _ = systemctl(&["disable", "--now", "rustwtm.service"]);
    if Path::new(ADMIN_UNIT).exists() {
        std::fs::remove_file(ADMIN_UNIT).with_context(|| format!("removing {ADMIN_UNIT}"))?;
        let _ = systemctl(&["daemon-reload"]);
    }
    println!("Service '{SERVICE_NAME}' removed.");
    cleanup()
}

/// Removes what RustWTM leaves outside its own files: network blocks
/// (nftables table and scopes) and a power profile game mode left
/// switched. Program blocks end with the service by themselves.
pub fn cleanup() -> anyhow::Result<()> {
    engine::restore_saved_plan(
        &Paths::under(&engine::default_data_dir()).power_plan,
        &mut |line| println!("{line}"),
    );
    let mut failures = 0;
    let (removed, failed) = Blocker::detached().remove_all();
    for name in removed {
        println!("No longer blocked from starting: {name}");
    }
    failures += failed.len();
    match wtm_sys::firewall::unblock_all() {
        Ok(0) => {}
        Ok(n) => println!("Removed {n} RustWTM network block(s)."),
        Err(e) => {
            println!("Could not remove RustWTM network blocks: {e}");
            failures += 1;
        }
    }
    anyhow::ensure!(failures == 0, "{failures} item(s) could not be removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_the_service_as_root_and_spares_moved_processes() {
        let t = unit_text(Path::new("/usr/bin/wtm-service"));
        assert!(t.contains("ExecStart=/usr/bin/wtm-service service"));
        assert!(t.contains("KillMode=mixed"));
        assert!(t.contains("WantedBy=multi-user.target"));
    }
}
