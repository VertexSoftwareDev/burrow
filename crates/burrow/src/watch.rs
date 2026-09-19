//! Keeping the map true to the disk while it is on screen.
//!
//! A scan is a snapshot; a minute later a download has finished and a build
//! has written ten thousand files. Rather than rescan, the watcher tails the
//! NTFS change journal, folds each change into the index, and rebuilds the
//! folder totals — so a file deleted in Explorer leaves the map on its own.
//!
//! # Not losing the scan's own seconds
//!
//! The journal position is taken *before* the scan starts, not after it
//! ends. Anything that changed while the table was being read is then
//! replayed; applying a change the scan already saw is harmless, and a change
//! it missed is not lost.
//!
//! # Never freezing the window
//!
//! The window reads the scan every frame, so no lock is held for long:
//!
//! 1. file sizes are read through the filesystem with no lock held;
//! 2. the changes go into the index under a brief write lock;
//! 3. the tree is rebuilt under a *read* lock — the window keeps drawing;
//! 4. the finished tree is swapped in under a brief write lock.
//!
//! Between 2 and 4 the index is ahead of the tree. That is safe: the tree
//! only ever asks the index for names of nodes it already knows.
//!
//! The watcher holds the scan weakly. When a rescan replaces it, the next
//! poll finds nothing to upgrade and the thread ends on its own.

use std::collections::HashMap;
use std::sync::{RwLock, Weak};
use std::time::{Duration, Instant};

use burrow_tree::Tree;
use ferret_core::journal::{self, Change, Cursor};
use ferret_core::Volume;

use crate::shell;
use crate::worker::Scan;

/// How often the journal is read.
const POLL: Duration = Duration::from_millis(700);
/// Totals are recomputed at most this often while changes keep coming.
const REBUILD_EVERY: Duration = Duration::from_millis(2_000);
/// Bound on changes held before they are applied, so a mass operation — an
/// archive unpacking, a Windows update — is folded in in steps.
const MAX_PENDING: usize = 50_000;

pub enum WatchEvent {
    /// Changes landed; the scan's generation has moved.
    Updated,
    /// The journal wrapped or was reset: the snapshot can no longer be
    /// patched and only a rescan will do.
    Stale,
}

pub fn spawn(
    scan: Weak<RwLock<Scan>>,
    letter: char,
    cursor: Cursor,
    sink: impl Fn(WatchEvent) + Send + 'static,
) {
    let _ = std::thread::Builder::new()
        .name(format!("burrow-watch-{letter}"))
        .spawn(move || run(scan, letter, cursor, &sink));
}

fn run(scan: Weak<RwLock<Scan>>, letter: char, mut cursor: Cursor, sink: &dyn Fn(WatchEvent)) {
    let Ok(volume) = Volume::open(letter) else {
        return;
    };
    let mut pending: Vec<Change> = Vec::new();
    let mut last_apply = Instant::now();

    loop {
        std::thread::sleep(POLL);
        let Some(shared) = scan.upgrade() else {
            return; // replaced by a rescan, or the window closed
        };

        while pending.len() < MAX_PENDING {
            match journal::read(&volume, &mut cursor) {
                Ok(changes) if changes.is_empty() => break,
                Ok(changes) => pending.extend(changes),
                Err(_) => {
                    if let Ok(mut s) = shared.write() {
                        s.live = false;
                    }
                    sink(WatchEvent::Stale);
                    return;
                }
            }
        }

        let due = last_apply.elapsed() >= REBUILD_EVERY || pending.len() >= MAX_PENDING;
        if pending.is_empty() || !due {
            continue;
        }
        last_apply = Instant::now();
        let batch = collapse(std::mem::take(&mut pending));
        if apply(&shared, letter, &batch) {
            sink(WatchEvent::Updated);
        }
    }
}

/// One save produces several entries for the same file — create, extend,
/// close. Only the last state matters; keep it, in first-seen order so a
/// folder is created before what goes into it.
fn collapse(batch: Vec<Change>) -> Vec<Change> {
    let mut last: HashMap<u32, usize> = HashMap::with_capacity(batch.len());
    let mut order: Vec<u32> = Vec::with_capacity(batch.len());
    for (i, change) in batch.iter().enumerate() {
        if last.insert(change.record, i).is_none() {
            order.push(change.record);
        }
    }
    let mut slots: Vec<Option<Change>> = batch.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|record| slots[last[&record]].take())
        .collect()
}

/// Fold changes into the scan. Returns whether anything moved.
fn apply(shared: &RwLock<Scan>, letter: char, batch: &[Change]) -> bool {
    // 1. Paths under a read lock, sizes with none.
    let paths: Vec<Option<String>> = {
        let Ok(scan) = shared.read() else {
            return false;
        };
        batch
            .iter()
            .map(|change| {
                // A folder's own size never changes by being touched; keep
                // what the scan found.
                if change.is_delete() || change.is_dir() {
                    return None;
                }
                let parent = scan.index.by_record(change.parent).map(|(p, _)| p)?;
                let folder = scan.index.full_path(parent)?;
                Some(format!("{folder}\\{}", change.name))
            })
            .collect()
    };
    let stats: Vec<_> = paths
        .iter()
        .map(|p| p.as_deref().and_then(shell::stat))
        .collect();

    // 2. The index, under a brief write lock.
    let applied = {
        let Ok(mut scan) = shared.write() else {
            return false;
        };
        let mut applied = 0u64;
        for (change, stat) in batch.iter().zip(stats) {
            if scan.index.apply_change(change, stat).changed {
                applied += 1;
            }
        }
        scan.changes += applied;
        applied
    };
    if applied == 0 {
        return false;
    }

    // 3. The tree, under a read lock the window can share.
    let tree = {
        let Ok(scan) = shared.read() else {
            return false;
        };
        Tree::build(&scan.index)
    };

    // 4. The swap.
    let drive = shell::drive(letter);
    let Ok(mut scan) = shared.write() else {
        return false;
    };
    scan.tree = tree;
    scan.generation += 1;
    if let Some(drive) = drive {
        scan.drive = drive;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(record: u32, name: &str, reason: u32) -> Change {
        Change {
            record,
            parent: 5,
            name: name.into(),
            reason,
            attributes: 0,
        }
    }

    #[test]
    fn collapsing_keeps_the_last_state_in_first_seen_order() {
        let batch = vec![
            change(30, "a.tmp", journal::REASON_FILE_CREATE),
            change(31, "b.txt", journal::REASON_FILE_CREATE),
            change(30, "a.tmp", journal::REASON_DATA_EXTEND),
            change(30, "a.txt", journal::REASON_RENAME_NEW_NAME),
        ];
        let kept = collapse(batch);
        assert_eq!(kept.len(), 2);
        assert_eq!((kept[0].record, kept[0].name.as_str()), (30, "a.txt"));
        assert_eq!(kept[1].record, 31);
    }
}
