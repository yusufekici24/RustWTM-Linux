//! The NVMe SMART / health log (log page 0x02), read with a fixed-size
//! Get Log Page admin command on the controller character device. Read
//! only: no other admin command is ever issued. Needs root.

use std::os::fd::AsRawFd;

#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub critical: u8,
    pub temperature: Option<f64>,
    pub spare: Option<u8>,
    pub used: u8,
    pub read_units: u128,
    pub written_units: u128,
    pub power_cycles: u128,
    pub hours: u128,
    pub unsafe_shutdowns: u128,
    pub media_errors: u128,
}

pub fn parse(data: &[u8]) -> Result<Health, String> {
    if data.len() != 512 || data.iter().all(|b| *b == 0) || data.iter().all(|b| *b == 255) {
        return Err("NVMe health log is unavailable or invalid".into());
    }
    let count = |offset| {
        u128::from_le_bytes(
            data[offset..offset + 16]
                .try_into()
                .expect("fixed validated log"),
        )
    };
    let kelvin = u16::from_le_bytes([data[1], data[2]]);
    Ok(Health {
        critical: data[0],
        temperature: (200..=1000)
            .contains(&kelvin)
            .then_some(f64::from(kelvin) - 273.15),
        spare: (data[3] <= 100).then_some(data[3]),
        used: data[5],
        read_units: count(32),
        written_units: count(48),
        power_cycles: count(112),
        hours: count(128),
        unsafe_shutdowns: count(144),
        media_errors: count(160),
    })
}

/// `struct nvme_admin_cmd` from <linux/nvme_ioctl.h>.
#[repr(C)]
#[derive(Default)]
struct AdminCmd {
    opcode: u8,
    flags: u8,
    rsvd1: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    metadata: u64,
    addr: u64,
    metadata_len: u32,
    data_len: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
    timeout_ms: u32,
    result: u32,
}

/// `_IOWR('N', 0x41, struct nvme_admin_cmd)`.
const NVME_IOCTL_ADMIN_CMD: libc::c_ulong = 0xC048_4E41;

pub fn read(number: u32) -> Result<Health, String> {
    let disk = crate::disk::disk_name(number).ok_or("Invalid physical disk number")?;
    if !disk.starts_with("nvme") {
        return Err("Not an NVMe drive".into());
    }
    // nvme0n1 -> /dev/nvme0 (the controller).
    let controller = match disk.rfind('n') {
        Some(i) if i > 4 => &disk[..i],
        _ => disk.as_str(),
    };
    let file = std::fs::File::open(format!("/dev/{controller}"))
        .map_err(|e| format!("Opening /dev/{controller}: {e}"))?;
    let mut data = [0u8; 512];
    // Log page 0x02, all namespaces, NUMD = 512/4 - 1 in the lower half.
    let numd: u32 = (512 / 4) - 1;
    let mut cmd = AdminCmd {
        opcode: 0x02,
        nsid: 0xFFFF_FFFF,
        addr: data.as_mut_ptr() as u64,
        data_len: data.len() as u32,
        cdw10: 0x02 | ((numd & 0xFFFF) << 16),
        cdw11: numd >> 16,
        timeout_ms: 2000,
        ..Default::default()
    };
    // SAFETY: cmd is a correctly laid out nvme_admin_cmd whose buffer
    // (data) is valid for data_len bytes for the whole call.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), NVME_IOCTL_ADMIN_CMD as _, &mut cmd) };
    if rc != 0 {
        return Err(format!(
            "NVMe Get Log Page failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    parse(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_log_decodes_units_and_does_not_invent_temperature() {
        let mut b = [0u8; 512];
        b[1..3].copy_from_slice(&300u16.to_le_bytes());
        b[3] = 99;
        b[5] = 120;
        b[32..48].copy_from_slice(&123u128.to_le_bytes());
        let h = parse(&b).unwrap();
        assert!((h.temperature.unwrap() - 26.85).abs() < 0.001);
        assert_eq!(h.used, 120);
        assert_eq!(h.read_units, 123);
        assert!(parse(&[0u8; 512]).is_err());
    }

    #[test]
    fn admin_cmd_layout_matches_the_kernel() {
        assert_eq!(size_of::<AdminCmd>(), 72);
    }
}
