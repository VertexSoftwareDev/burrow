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
//! What is never *looked at*: every place [`crate::cleanup::is_protected`]
//! guards — Windows, installed programs, the Recycle Bin, System Volume
//! Information. Identical contents there are not waste. Windows keeps
//! `Sessions.xml` beside `Sessions.back.xml` on purpose, to recover from an
//! update that fails halfway; a game ships the same asset in two packages
//! because it loads them separately; the Recycle Bin holds copies of what
//! was just removed as duplicates. None of it can be removed from here, so
//! none of it is counted as space that could be freed.

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
}

/// Files with identical contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub size: u64,
    pub files: Vec<Candidate>,
}

impl Group {
    /// Space that deleting all but one copy would free.
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64).saturating_sub(1)
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
pub fn candidates(index: &Index, tree: &Tree, min_size: u64) -> Vec<Candidate> {
    let entries = index.entries();
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
            // Checked last: it walks the parent chain, and the size test
            // above has already ruled out almost every file.
            if skip || crate::cleanup::is_protected(index, tree, node) {
                return None;
            }
            Some(Candidate {
                node,
                path: tree.path(index, node),
                size: entry.size,
            })
        })
        .collect();
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

/// Whether a path lies inside an application's own folder: `AppData`, or a
/// dot-folder such as `.minecraft`, `.gradle` or `.lmstudio`.
///
/// Identical contents do not make such a copy redundant. The application
/// reads it from that exact path — a launcher's `versions\1.21\1.21.jar`,
/// an app's bundled tool, an editor's copy of a video it has imported — and
/// removing it breaks the application even though the same bytes survive
/// elsewhere. These copies are shown, but never ticked on anyone's behalf.
pub fn owned_by_an_app(path: &str) -> bool {
    path.split('\\').any(|part| {
        part.eq_ignore_ascii_case("appdata") || (part.starts_with('.') && part.len() > 1)
    })
}

/// Whether "keep one, remove the rest" may be applied to a group without
/// asking: only when every copy sits in the person's own folders. A group
/// with even one application-owned copy is theirs to decide file by file —
/// removing the others could leave only the copy an app hides away.
pub fn safe_to_thin(group: &Group) -> bool {
    !group.files.iter().any(|f| owned_by_an_app(&f.path))
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

    groups.sort_by(|a, b| {
        b.wasted()
            .cmp(&a.wasted())
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
            file(20, 31, "keep.bin").sized(2 << 20),
            file(21, 31, "tiny.txt").sized(10),
            file(22, 31, "online.mp4")
                .sized(5 << 20)
                .with_flags(IS_CLOUD),
            file(3, ROOT_RECORD, "$Volume").sized(1 << 20),
        ]);
        let tree = Tree::build(&index);
        let found = candidates(&index, &tree, 1 << 20);
        let names: Vec<_> = found.iter().map(|c| index.name(c.node as usize)).collect();
        assert_eq!(names, ["keep.bin"]);
    }

    #[test]
    fn copies_an_application_uses_are_not_thinned_automatically() {
        let group = |paths: &[&str]| Group {
            size: 1,
            files: paths
                .iter()
                .enumerate()
                .map(|(i, p)| Candidate {
                    node: i as NodeId,
                    path: p.to_string(),
                    size: 1,
                })
                .collect(),
        };
        // A launcher loads each version's jar from its own folder.
        assert!(!safe_to_thin(&group(&[
            r"C:\Users\pc\AppData\Roaming\.minecraft\versions\Forge 1.21.11\Forge 1.21.11.jar",
            r"C:\Users\pc\AppData\Roaming\.minecraft\versions\aa2\aa2.jar",
        ])));
        // A video in Downloads and the copy an editor imported into its own
        // storage: removing the first would leave only the hidden one.
        assert!(!safe_to_thin(&group(&[
            r"C:\Users\pc\Downloads\SubVizion_V2.mp4",
            r"C:\Users\pc\AppData\Local\Packages\Clipchamp\LocalState\00000014",
        ])));
        assert!(!safe_to_thin(&group(&[
            r"C:\Users\pc\.lmstudio\bin\lms.exe",
            r"C:\Users\pc\lms.exe"
        ])));
        // The same photo downloaded twice is exactly what the button is for.
        assert!(safe_to_thin(&group(&[
            r"C:\Users\pc\Downloads\IMG_2031.jpg",
            r"C:\Users\pc\Pictures\Holiday\IMG_2031.jpg",
        ])));
        assert!(owned_by_an_app(r"C:\Users\pc\.gradle\caches\x.jar"));
        assert!(!owned_by_an_app(r"C:\Users\pc\Documents\report.pdf"));
    }

    #[test]
    fn protected_places_are_never_candidates() {
        use ferret_core::testing::{dir, file, index_from_specs};
        use ferret_core::ROOT_RECORD;
        let index = index_from_specs(vec![
            // Windows keeps this pair on purpose, to recover a failed update.
            dir(30, ROOT_RECORD, "Windows"),
            dir(31, 30, "servicing"),
            dir(32, 31, "Sessions"),
            file(33, 32, "Sessions.xml").sized(170 << 20),
            file(34, 32, "Sessions.back.xml").sized(170 << 20),
            // What was just recycled as a duplicate is not a duplicate again.
            dir(40, ROOT_RECORD, "$Recycle.Bin"),
            dir(41, 40, "S-1-5-21-1002"),
            file(42, 41, "$RK7W92H.jar").sized(30 << 20),
            dir(50, ROOT_RECORD, "Program Files"),
            file(51, 50, "asset.pak").sized(30 << 20),
            dir(60, ROOT_RECORD, "Users"),
            dir(61, 60, "pc"),
            file(62, 61, "client.jar").sized(30 << 20),
        ]);
        let tree = Tree::build(&index);
        let found = candidates(&index, &tree, 1 << 20);
        let names: Vec<_> = found.iter().map(|c| index.name(c.node as usize)).collect();
        assert_eq!(names, ["client.jar"]);
    }
}
