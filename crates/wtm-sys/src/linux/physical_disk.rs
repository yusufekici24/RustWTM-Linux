//! Identity and temperature of a physical disk from sysfs: model, bus,
//! SSD or HDD, capacity; temperature from the drive's hwmon (the nvme
//! driver and drivetemp for SATA).

use std::fs;

use crate::linux::util::{read_trim, read_u64};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhysicalDisk {
    pub number: u32,
    pub model: String,
    /// `STORAGE_BUS_TYPE` code (17 NVMe, 11 SATA, 7 USB, ...).
    pub bus: u32,
    pub removable: bool,
    pub ssd: Option<bool>,
    pub size: Option<u64>,
}

impl PhysicalDisk {
    pub fn bus_name(&self) -> Option<&'static str> {
        Some(match self.bus {
            1 => "SCSI",
            2 => "ATAPI",
            3 => "ATA",
            4 => "IEEE 1394",
            5 => "SSA",
            6 => "Fibre Channel",
            7 => "USB",
            8 => "RAID",
            9 => "iSCSI",
            10 => "SAS",
            11 => "SATA",
            12 => "SD",
            13 => "MMC",
            14 => "Virtual",
            15 => "File-backed virtual",
            16 => "Storage Spaces",
            17 => "NVMe",
            18 => "SCM",
            19 => "UFS",
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiskTemperature {
    pub celsius: f32,
    pub warning: Option<f32>,
    pub critical: Option<f32>,
}

fn bus(disk: &str) -> u32 {
    let path = fs::canonicalize(format!("/sys/block/{disk}"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if path.contains("/usb") {
        7
    } else if disk.starts_with("nvme") {
        17
    } else if disk.starts_with("mmcblk") {
        if read_trim(format!("/sys/block/{disk}/device/type")).as_deref() == Some("SD") {
            12
        } else {
            13
        }
    } else if disk.starts_with("vd") || disk.starts_with("xvd") || path.contains("/virtio") {
        14
    } else if path.contains("/ata") {
        11
    } else if disk.starts_with("sd") {
        1
    } else {
        0
    }
}

pub(crate) fn model(disk: &str) -> String {
    let dev = format!("/sys/block/{disk}/device");
    // virtio and NVMe report a PCI vendor ID ("0x1af4") here, not a name.
    let vendor = read_trim(format!("{dev}/vendor"))
        .filter(|v| !v.starts_with("0x"))
        .unwrap_or_default();
    let model = read_trim(format!("{dev}/model"))
        .or_else(|| read_trim(format!("{dev}/name")))
        .unwrap_or_default();
    // SATA disks report vendor "ATA"; it is the bus, not the maker.
    let text = if vendor.is_empty() || vendor == "ATA" || model.starts_with(&vendor) {
        model
    } else {
        format!("{vendor} {model}")
    };
    if !text.is_empty() {
        return text;
    }
    if disk.starts_with("vd") || disk.starts_with("xvd") {
        return "Virtual disk".into();
    }
    disk.to_owned()
}

pub fn read(number: u32) -> Result<PhysicalDisk, String> {
    let disk = crate::disk::disk_name(number).ok_or("No such physical disk")?;
    let rotational = read_u64(format!("/sys/block/{disk}/queue/rotational"));
    Ok(PhysicalDisk {
        number,
        model: model(&disk),
        bus: bus(&disk),
        removable: crate::volumes::removable(&disk),
        ssd: rotational.map(|r| r == 0),
        size: read_u64(format!("/sys/block/{disk}/size")).map(|s| s * 512),
    })
}

pub(crate) fn hwmon_dirs(disk: &str) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    for base in [
        format!("/sys/block/{disk}/device"),
        format!("/sys/block/{disk}/device/device"),
    ] {
        if let Ok(rd) = fs::read_dir(format!("{base}/hwmon")) {
            dirs.extend(rd.flatten().map(|e| e.path()));
        }
        // nvme0n1/device is the controller; its hwmon sits there.
        if let Ok(rd) = fs::read_dir(&base) {
            dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("hwmon"))
            }));
        }
    }
    dirs
}

pub fn temperature(number: u32) -> Result<DiskTemperature, String> {
    let disk = crate::disk::disk_name(number).ok_or("No such physical disk")?;
    for dir in hwmon_dirs(&disk) {
        let milli = |leaf: &str| {
            read_trim(dir.join(leaf))
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|v| *v > -40_000 && *v < 200_000)
                .map(|v| v as f32 / 1000.0)
        };
        if let Some(celsius) = milli("temp1_input") {
            return Ok(DiskTemperature {
                celsius,
                warning: milli("temp1_max"),
                critical: milli("temp1_crit"),
            });
        }
    }
    Err("The drive reports no temperature".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_buses() {
        let d = PhysicalDisk {
            bus: 17,
            ..Default::default()
        };
        assert_eq!(d.bus_name(), Some("NVMe"));
    }

    #[test]
    fn reads_first_disk_if_any() {
        if crate::disk::disks().is_empty() {
            return;
        }
        let d = read(0).unwrap();
        assert!(!d.model.is_empty());
    }
}
