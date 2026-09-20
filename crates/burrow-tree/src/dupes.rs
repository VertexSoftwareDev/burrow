//! Finding files whose contents are identical.
//!
//! Reading every byte of a disk to compare it would take as long as copying
//! the disk. Almost none of that reading is needed, because almost no two
//! files are candidates:
//!
//! 1. **Size.** Files of different sizes cannot be equal, and the index
//!    already knows every size — this step reads nothing, and on a system
//!    disk it rules out the great majority of files.
//! 2. **Head and tail.** Of what is left, most same-size files differ in
//!    their first or last 64 KB. Hashing those two blocks rules them out
//!    after reading 128 KB each.
//! 3. **Everything.** Only files that still agree are read in full.
//!
//! Hashes are SHA-256: a disk full of files is exactly the setting where a
//! weak hash's collisions stop being theoretical, and the reading, not the
//! hashing, is what takes the time.
//!
//! What is never read: cloud-only placeholders (reading one would download
//! it), reparse points, NTFS's own metafiles. Hard links cannot show up as
//! duplicates of each other — the index holds one entry per file, not per
//! name.
//!
//! # What may go, and what only explains itself
//!
//! Most of what a disk wastes on duplicates is not the person's to remove:
//! a launcher keeps the same jar under every version it has, a browser
//! caches the same asset in six profiles, Windows keeps `Sessions.xml`
//! beside `Sessions.back.xml` on purpose. Hiding all of that would answer
//! "where did my space go?" with silence.
//!
//! So every copy is listed, and each carries whether
//! [`crate::safety::Policy`] would let it go ([`Candidate::removable`]).
//! What it refuses is shown locked, counted in [`Group::wasted`] — the
//! waste is real — but never in [`Group::reclaimable`], which is what
//! Burrow itself could free.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ferret_core::Index;
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::{NodeId, Tree};

/// Bytes hashed from each end of a file in the second stage.
pub const EDGE: u64 = 64 * 1024;

pub type Hash = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub node: NodeId,
    pub path: String,
    pub size: u64,
    /// Whether the safety policy would let this copy go. Copies it refuses
    /// are still listed — most of what a disk wastes on duplicates sits in
    /// application folders, and seeing where it went is worth something
    /// even when the answer is "leave it alone".
    pub removable: bool,
}

/// Files with identical contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub size: u64,
    pub files: Vec<Candidate>,
}

impl Group {
    /// Space that deleting all but one copy would free, whoever could do it.
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64).saturating_sub(1)
    }

    /// Of that, what Burrow could actually free: the copies the safety
    /// policy allows, never counting the last copy of the group.
    pub fn reclaimable(&self) -> u64 {
        let removable = self.files.iter().filter(|f| f.removable).count() as u64;
        self.size * removable.min(self.files.len() as u64 - 1)
    }

    /// Whether any copy here is one Burrow may remove.
    pub fn has_removable(&self) -> bool {
        self.files.iter().any(|f| f.removable)
    }
}

/// How far a search has got.
#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    pub bytes_done: u64,
    /// Upper bound: what would be read if every candidate reached the last
    /// stage. Most drop out early, so the bar tends to jump to the end.
    pub bytes_total: u64,
    pub groups: usize,
}

/// Where file contents come from. The real one reads the disk; tests hand
/// in bytes.
pub trait Source: Sync {
    fn edges(&self, path: &str, size: u64) -> io::Result<Hash>;
    fn whole(&self, path: &str, counted: &AtomicU64, cancel: &AtomicBool) -> io::Result<Hash>;
}

/// Every file in the tree worth comparing, largest first.
pub fn candidates(index: &Index, tree: &Tree, min_size: u64, profile: &str) -> Vec<Candidate> {
    let entries = index.entries();
    let policy = crate::safety::Policy::new(index, tree, profile);
    let mut out: Vec<Candidate> = (0..tree.root())
        .filter(|node| tree.contains(*node) && !tree.is_dir(*node))
        .filter_map(|node| {
            let entry = &entries[node as usize];
            let skip = entry.size < min_size.max(1)
                || entry.is_cloud()
                || entry.is_reparse()
                // Sparse or not really stored: nothing to compare, or
                // reading would fetch it from somewhere.
                || entry.allocated == 0
                || entry.record < 16
                || index.name_of(entry).starts_with('$') && entry.parent == ferret_core::ROOT_RECORD;
            if skip {
                return None;
            }
            Some(Candidate {
                node,
                path: tree.path(index, node),
                size: entry.size,
                // Walks the parent chain, so it is asked last: the size test
                // above has already ruled out almost every file.
                removable: policy.allows(node),
            })
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.size));
    out
}

/// Run the three stages. Files that cannot be read (in use, protected,
/// deleted meanwhile) simply drop out.
pub fn find(
    candidates: Vec<Candidate>,
    source: &dyn Source,
    cancel: &AtomicBool,
    progress: &(dyn Fn(Progress) + Sync),
) -> Vec<Group> {
    // Stage 1: size.
    let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for c in candidates {
        by_size.entry(c.size).or_default().push(c);
    }
    let same_size: Vec<Vec<Candidate>> = by_size.into_values().filter(|g| g.len() > 1).collect();

    let total: u64 = same_size
        .iter()
        .flat_map(|g| g.iter())
        .map(|c| c.size)
        .sum();
    let done = AtomicU64::new(0);
    let report = |groups: usize| {
        progress(Progress {
            bytes_done: done.load(Ordering::Relaxed).min(total),
            bytes_total: total,
            groups,
        })
    };
    report(0);

    // Stage 2: head and tail, one same-size group per task.
    let edged: Vec<Vec<Candidate>> = same_size
        .into_par_iter()
        .flat_map_iter(|group| {
            if cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let size = group[0].size;
            let split = split_by(group, |c| source.edges(&c.path, c.size).ok());
            done.fetch_add(
                size.min(2 * EDGE) * split.iter().map(|g| g.len() as u64).sum::<u64>(),
                Ordering::Relaxed,
            );
            split
        })
        .collect();
    report(0);

    // Stage 3: whole contents, for groups the edges could not tell apart.
    // A file no larger than both edges was already read in full.
    let found = AtomicU64::new(0);
    let mut groups: Vec<Group> = edged
        .into_par_iter()
        .flat_map_iter(|group| {
            if cancel.load(Ordering::Relaxed) {
                return Vec::new();
            }
            let size = group[0].size;
            let equal = if size <= 2 * EDGE {
                vec![group]
            } else {
                split_by(group, |c| source.whole(&c.path, &done, cancel).ok())
            };
            let n = found.fetch_add(equal.len() as u64, Ordering::Relaxed) as usize + equal.len();
            report(n);
            equal
                .into_iter()
                .map(|files| Group { size, files })
                .collect::<Vec<_>>()
        })
        .collect();

    // What can be acted on first, then the plain size of the waste.
    groups.sort_by(|a, b| {
        b.has_removable()
            .cmp(&a.has_removable())
            .then(b.reclaimable().cmp(&a.reclaimable()))
            .then(b.wasted().cmp(&a.wasted()))
            .then(a.files[0].path.cmp(&b.files[0].path))
    });
    groups
}

/// Split a group by a key, keeping the sub-groups of two or more. Files
/// whose key could not be computed are dropped.
fn split_by(
    group: Vec<Candidate>,
    key: impl Fn(&Candidate) -> Option<Hash> + Sync,
) -> Vec<Vec<Candidate>> {
    // The files of one group are hashed in parallel too: ten copies of the
    // same installer should not be read one after another.
    let keyed: Vec<(Option<Hash>, Candidate)> =
        group.into_par_iter().map(|c| (key(&c), c)).collect();
    let mut by_key: HashMap<Hash, Vec<Candidate>> = HashMap::new();
    for (k, c) in keyed {
        if let Some(k) = k {
            by_key.entry(k).or_default().push(c);
        }
    }
    by_key.into_values().filter(|g| g.len() > 1).collect()
}

/// Reads through the filesystem.
pub struct Disk;

impl Source for Disk {
    fn edges(&self, path: &str, size: u64) -> io::Result<Hash> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; EDGE as usize];
        if size <= 2 * EDGE {
            let mut all = Vec::with_capacity(size as usize);
            file.read_to_end(&mut all)?;
            hasher.update(&all);
        } else {
            file.read_exact(&mut buffer)?;
            hasher.update(&buffer);
            file.seek(SeekFrom::Start(size - EDGE))?;
            file.read_exact(&mut buffer)?;
            hasher.update(&buffer);
        }
        Ok(hasher.finalize().into())
    }

    fn whole(&self, path: &str, counted: &AtomicU64, cancel: &AtomicBool) -> io::Result<Hash> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 20];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            counted.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(hasher.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    /// Files as bytes in memory.
    struct Memory(Map<String, Vec<u8>>);

    impl Source for Memory {
        fn edges(&self, path: &str, size: u64) -> io::Result<Hash> {
            let data = self.0.get(path).ok_or(io::ErrorKind::NotFound)?;
            let mut h = Sha256::new();
            if size <= 2 * EDGE {
                h.update(data);
            } else {
                h.update(&data[..EDGE as usize]);
                h.update(&data[data.len() - EDGE as usize..]);
            }
            Ok(h.finalize().into())
        }
        fn whole(&self, path: &str, counted: &AtomicU64, _: &AtomicBool) -> io::Result<Hash> {
            let data = self.0.get(path).ok_or(io::ErrorKind::NotFound)?;
            counted.fetch_add(data.len() as u64, Ordering::Relaxed);
            Ok(Sha256::digest(data).into())
        }
    }

    fn run(files: &[(&str, Vec<u8>)]) -> Vec<Group> {
        let source = Memory(
            files
                .iter()
                .map(|(p, d)| (p.to_string(), d.clone()))
                .collect(),
        );
        let candidates = files
            .iter()
            .enumerate()
            .map(|(i, (p, d))| Candidate {
                node: i as NodeId,
                path: p.to_string(),
                size: d.len() as u64,
                removable: true,
            })
            .collect();
        find(candidates, &source, &AtomicBool::new(false), &|_| {})
    }

    fn names(group: &Group) -> Vec<&str> {
        let mut n: Vec<&str> = group.files.iter().map(|c| c.path.as_str()).collect();
        n.sort();
        n
    }

    #[test]
    fn identical_files_are_grouped_and_ranked_by_waste() {
        let big = vec![7u8; 300_000];
        let small = vec![1u8; 1000];
        let groups = run(&[
            ("a.iso", big.clone()),
            ("b.iso", big.clone()),
            ("c.iso", big),
            ("x.txt", small.clone()),
            ("y.txt", small),
            ("lonely.bin", vec![9u8; 300_000]),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(names(&groups[0]), ["a.iso", "b.iso", "c.iso"]);
        assert_eq!(groups[0].wasted(), 600_000);
        assert_eq!(names(&groups[1]), ["x.txt", "y.txt"]);
    }

    #[test]
    fn same_edges_but_a_different_middle_are_not_duplicates() {
        // Large enough that the middle is never seen by the edge hash.
        let mut a = vec![0u8; 400_000];
        let mut b = a.clone();
        a[200_000] = 1;
        b[200_000] = 2;
        assert!(run(&[("a.bin", a), ("b.bin", b)]).is_empty());
    }

    #[test]
    fn same_size_different_start_is_ruled_out() {
        let a = vec![1u8; 500_000];
        let b = vec![2u8; 500_000];
        assert!(run(&[("a", a), ("b", b)]).is_empty());
    }

    #[test]
    fn unreadable_files_drop_out_without_breaking_the_group() {
        let data = vec![3u8; 200_000];
        let source = Memory(
            [("a".to_string(), data.clone()), ("b".to_string(), data)]
                .into_iter()
                .collect(),
        );
        let candidates = ["a", "b", "gone"]
            .iter()
            .enumerate()
            .map(|(i, p)| Candidate {
                node: i as NodeId,
                path: p.to_string(),
                size: 200_000,
                removable: true,
            })
            .collect();
        let groups = find(candidates, &source, &AtomicBool::new(false), &|_| {});
        assert_eq!(groups.len(), 1);
        assert_eq!(names(&groups[0]), ["a", "b"]);
    }

    #[test]
    fn a_cancelled_search_returns_nothing() {
        let data = vec![3u8; 200_000];
        let source = Memory(
            [("a".into(), data.clone()), ("b".into(), data)]
                .into_iter()
                .collect(),
        );
        let candidates = ["a", "b"]
            .iter()
            .enumerate()
            .map(|(i, p)| Candidate {
                node: i as NodeId,
                path: p.to_string(),
                size: 200_000,
                removable: true,
            })
            .collect();
        assert!(find(candidates, &source, &AtomicBool::new(true), &|_| {}).is_empty());
    }

    #[test]
    fn candidates_skip_small_cloud_and_metafiles() {
        use ferret_core::mft::IS_CLOUD;
        use ferret_core::testing::{dir, file, index_from_specs};
        use ferret_core::ROOT_RECORD;
        let index = index_from_specs(vec![
            dir(30, ROOT_RECORD, "Users"),
            dir(31, 30, "pc"),
            dir(32, 31, "Documents"),
            file(20, 32, "keep.bin").sized(2 << 20),
            file(21, 32, "tiny.txt").sized(10),
            file(22, 32, "online.mp4")
                .sized(5 << 20)
                .with_flags(IS_CLOUD),
            file(3, ROOT_RECORD, "$Volume").sized(1 << 20),
        ]);
        let tree = Tree::build(&index);
        let found = candidates(&index, &tree, 1 << 20, "pc");
        let names: Vec<_> = found.iter().map(|c| index.name(c.node as usize)).collect();
        assert_eq!(names, ["keep.bin"]);
    }

    #[test]
    fn protected_places_are_listed_but_locked() {
        use ferret_core::testing::{dir, file, index_from_specs};
        use ferret_core::ROOT_RECORD;
        let index = index_from_specs(vec![
            // Windows keeps this pair on purpose, to recover a failed update.
            dir(30, ROOT_RECORD, "Windows"),
            dir(31, 30, "servicing"),
            dir(32, 31, "Sessions"),
            file(33, 32, "Sessions.xml").sized(170 << 20),
            file(34, 32, "Sessions.back.xml").sized(170 << 20),
            dir(50, ROOT_RECORD, "Program Files"),
            file(51, 50, "asset.pak").sized(30 << 20),
            dir(60, ROOT_RECORD, "Users"),
            dir(61, 60, "pc"),
            dir(63, 61, "Downloads"),
            file(62, 63, "client.jar").sized(30 << 20),
            // The launcher's own copy: shown, never ticked.
            dir(64, 61, ".minecraft"),
            file(65, 64, "1.21.jar").sized(30 << 20),
        ]);
        let tree = Tree::build(&index);
        let found = candidates(&index, &tree, 1 << 20, "pc");
        let removable: Vec<_> = found
            .iter()
            .filter(|c| c.removable)
            .map(|c| index.name(c.node as usize))
            .collect();
        assert_eq!(removable, ["client.jar"]);
        // Everything is still listed, so the waste can be seen.
        assert_eq!(found.len(), 5);
    }

    #[test]
    fn only_copies_that_may_go_count_as_reclaimable() {
        let copy = |path: &str, removable: bool| Candidate {
            node: 0,
            path: path.to_string(),
            size: 30 << 20,
            removable,
        };
        // Windows' pair: real waste, none of it Burrow's to free.
        let locked = Group {
            size: 30 << 20,
            files: vec![
                copy(r"C:\Windows\servicing\Sessions\Sessions.xml", false),
                copy(r"C:\Windows\servicing\Sessions\Sessions.back.xml", false),
            ],
        };
        assert_eq!(locked.wasted(), 30 << 20);
        assert_eq!(locked.reclaimable(), 0);
        assert!(!locked.has_removable());

        // One copy in Downloads, one the launcher keeps: only the first can go.
        let mixed = Group {
            size: 30 << 20,
            files: vec![
                copy(r"C:\Users\pc\Downloads\client.jar", true),
                copy(r"C:\Users\pc\AppData\Roaming\.minecraft\1.21.jar", false),
            ],
        };
        assert_eq!(mixed.reclaimable(), 30 << 20);

        // Three of the person's own: two can go, the last one never.
        let mine = Group {
            size: 30 << 20,
            files: vec![
                copy(r"C:\Users\pc\Downloads\a.jar", true),
                copy(r"C:\Users\pc\Documents\a.jar", true),
                copy(r"C:\Users\pc\Desktop\a.jar", true),
            ],
        };
        assert_eq!(mine.reclaimable(), 60 << 20);
        assert_eq!(mine.reclaimable(), mine.wasted());
    }
}
