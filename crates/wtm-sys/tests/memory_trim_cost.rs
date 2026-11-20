//! Bounded working-set measurement. Trims only this test process.

#![cfg(windows)]

use std::hint::black_box;
use std::time::Instant;
use wtm_sys::control::{Access, ProcessHandle};
use wtm_sys::process::{ProcessSnapshotter, RawProcess};

const BUFFER_MIB: usize = 32;
const PAGE: usize = 4096;
const PAIRS: usize = 10;

fn sample(snapshots: &mut ProcessSnapshotter) -> RawProcess {
    snapshots
        .snapshot_where(|pid, _| pid == std::process::id())
        .unwrap()
        .pop()
        .unwrap()
}

fn touch(bytes: &mut [u8]) {
    for byte in bytes.iter_mut().step_by(PAGE) {
        *byte = byte.wrapping_add(1);
    }
    black_box(bytes);
}

#[test]
#[ignore = "bounded working-set timing measurement"]
fn memory_trim_cost() {
    let own_process = ProcessHandle::open(std::process::id(), Access::SET_QUOTA).unwrap();
    let mut snapshots = ProcessSnapshotter::new();
    let mut bytes = vec![0_u8; BUFFER_MIB << 20];
    let mib = |bytes| bytes as f64 / (1 << 20) as f64;
    touch(&mut bytes);
    sample(&mut snapshots);

    println!(
        "Mode,Working set before MiB,Working set after MiB,Working set revisited MiB,Private commit after MiB,Trim us,Revisit us,Revisit page faults"
    );
    for pair in 0..PAIRS {
        for trim in [pair % 2 == 1, pair % 2 == 0] {
            let before = sample(&mut snapshots);
            let trim_us = if trim {
                let started = Instant::now();
                own_process.trim_working_set().unwrap();
                started.elapsed().as_micros()
            } else {
                0
            };
            let after = sample(&mut snapshots);
            let started = Instant::now();
            touch(&mut bytes);
            let revisit_us = started.elapsed().as_micros();
            let revisited = sample(&mut snapshots);
            println!(
                "{},{:.2},{:.2},{:.2},{:.2},{trim_us},{revisit_us},{}",
                if trim { "trim" } else { "baseline" },
                mib(before.working_set),
                mib(after.working_set),
                mib(revisited.working_set),
                mib(after.private_bytes),
                revisited.page_faults.wrapping_sub(after.page_faults),
            );
        }
    }
    assert_eq!(bytes[0], 1 + u8::try_from(2 * PAIRS).unwrap());
}
