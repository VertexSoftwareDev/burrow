//! `ferret-disk --selftest-live report.txt`: prove, on the real disk, that
//! the map follows the disk without a rescan.
//!
//! Scans the system drive, starts the watcher, writes a file of known size
//! into the temp folder, and waits for the tree to account for it; then
//! moves it to the recycle bin and waits for it to leave. Every step, and PASS or FAIL, goes
//! to the report — the release build has no console to print to.

use std::io::Write;
use std::sync::mpsc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use ferret_core::journal;
use ferret_core::{ScanOptions, Volume};
use ferret_tree::Tree;

use crate::shell;
use crate::watch::{self, WatchEvent};
use crate::worker::Scan;

const PROBE_BYTES: usize = 8 << 20;
const WAIT: Duration = Duration::from_secs(12);

pub fn run(report: &std::path::Path) -> i32 {
    let mut out = String::new();
    let passed = live(&mut out).unwrap_or_else(|err| {
        out.push_str(&format!("error: {err}\n"));
        false
    });
    out.push_str(if passed { "PASS\n" } else { "FAIL\n" });
    if let Ok(mut file) = std::fs::File::create(report) {
        let _ = file.write_all(out.as_bytes());
    }
    if passed {
        0
    } else {
        1
    }
}

fn live(out: &mut String) -> Result<bool, String> {
    let temp = std::env::temp_dir();
    let letter = temp
        .to_string_lossy()
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase())
        .ok_or("no temp folder")?;

    let volume = Volume::open(letter).map_err(|e| e.to_string())?;
    let cursor = journal::cursor_at_end(&volume).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let options = ScanOptions {
        include_system: true,
    };
    let index = ferret_core::scan_with(letter, options).map_err(|e| e.to_string())?;
    let tree = Tree::build(&index);
    let before = tree.totals(tree.root()).allocated;
    out.push_str(&format!(
        "scanned {letter}: in {:.1}s, {} bytes on disk\n",
        started.elapsed().as_secs_f64(),
        before
    ));

    let shared = Arc::new(RwLock::new(Scan {
        letter,
        index,
        tree,
        drive: shell::drive(letter).ok_or("no drive info")?,
        seconds: 0.0,
        generation: 1,
        live: true,
        changes: 0,
    }));
    let (tx, rx) = mpsc::channel();
    watch::spawn(Arc::downgrade(&shared), letter, cursor, move |event| {
        let _ = tx.send(matches!(event, WatchEvent::Updated));
    });

    let probe = temp.join(format!("ferret-disk-probe-{}.bin", std::process::id()));
    let name = probe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::fs::write(&probe, vec![0x5Au8; PROBE_BYTES]).map_err(|e| e.to_string())?;
    out.push_str(&format!(
        "wrote {} ({PROBE_BYTES} bytes)\n",
        probe.display()
    ));

    let found = wait_for(&rx, &shared, |scan| {
        find(scan, &name).map(|(size, _)| size == PROBE_BYTES as u64)
    });
    let appeared = match found {
        Some(elapsed) => {
            let scan = shared.read().map_err(|_| "lock")?;
            let (size, allocated) = find(&scan, &name).unwrap_or((0, 0));
            let after = scan.tree.totals(scan.tree.root()).allocated;
            out.push_str(&format!(
                "appeared after {:.1}s: size {size}, on disk {allocated}; total grew by {}\n",
                elapsed.as_secs_f64(),
                after as i64 - before as i64
            ));
            size == PROBE_BYTES as u64 && allocated >= PROBE_BYTES as u64
        }
        None => {
            out.push_str("did not appear\n");
            false
        }
    };

    // Removed the way the window removes things: into the recycle bin.
    let recycled = shell::recycle(&[probe.to_string_lossy().into_owned()]);
    out.push_str(&format!(
        "recycled: {} of {} gone, aborted {}\n",
        recycled.gone, recycled.requested, recycled.aborted
    ));
    let gone = wait_for(&rx, &shared, |scan| Some(find(scan, &name).is_none()));
    let disappeared = match gone {
        Some(elapsed) => {
            out.push_str(&format!("gone after {:.1}s\n", elapsed.as_secs_f64()));
            true
        }
        None => {
            out.push_str("still there after deletion\n");
            false
        }
    };

    Ok(appeared && disappeared)
}

/// The probe's `(size, on disk)` if the tree currently holds it.
fn find(scan: &Scan, name: &str) -> Option<(u64, u64)> {
    let index = &scan.index;
    (0..scan.tree.root())
        .filter(|node| scan.tree.contains(*node))
        .find(|node| index.name(*node as usize) == name)
        .map(|node| {
            let entry = &index.entries()[node as usize];
            (entry.size, entry.allocated)
        })
}

/// Wait until `done` holds after an update, or give up.
fn wait_for(
    rx: &mpsc::Receiver<bool>,
    shared: &RwLock<Scan>,
    done: impl Fn(&Scan) -> Option<bool>,
) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < WAIT {
        let _ = rx.recv_timeout(Duration::from_millis(500));
        let scan = shared.read().ok()?;
        if done(&scan) == Some(true) {
            return Some(started.elapsed());
        }
    }
    None
}
