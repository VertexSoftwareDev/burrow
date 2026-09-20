//! What the disk looked like before, and what grew since.
//!
//! "Why is my disk full?" is often really "why is it fuller than last
//! week?". A snapshot answers that: the on-disk size of every folder of a
//! megabyte or more, a few megabytes on disk, saved after each scan. Put
//! next to today's tree, it shows where the gigabytes went.
//!
//! # Only where growth actually happened
//!
//! When `C:\Users\pc\AppData\Local\Docker` grows by 10 GB, so do `Local`,
//! `AppData`, `pc`, `Users` and `C:\` — a naive list would be those six
//! rows, the interesting one last. So a folder is reported only when no
//! single child accounts for most of its change: the list lands on the
//! folder where the bytes were written, and a folder whose growth is spread
//! over many children still appears as itself.

use std::collections::HashMap;

use ferret_core::Index;

use crate::{NodeId, Tree};

/// Folders smaller than this are not recorded; their changes are noise.
pub const MIN_FOLDER: u64 = 1 << 20;

/// A folder is not reported when one child carries this much of its change.
const CONCENTRATION: f64 = 0.8;

const MAGIC: &[u8; 8] = b"FDSNAP01";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub letter: char,
    /// Unix seconds.
    pub taken: u64,
    /// Used space on the volume at the time.
    pub used: u64,
    /// `(path, bytes on disk)` of every folder of [`MIN_FOLDER`] or more,
    /// the volume root included.
    pub folders: Vec<(String, u64)>,
}

/// One folder's change between two snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub before: u64,
    pub after: u64,
}

impl Change {
    pub fn delta(&self) -> i64 {
        self.after as i64 - self.before as i64
    }
}

/// Record the tree as it is now.
pub fn capture(index: &Index, tree: &Tree, used: u64, taken: u64) -> Snapshot {
    let root = tree.root();
    let mut folders = vec![(tree.path(index, root), tree.totals(root).allocated)];
    // Walk down only through folders big enough to record: a folder under
    // the threshold cannot contain one over it.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        for &child in tree.children(node) {
            if !tree.is_dir(child) {
                continue;
            }
            let bytes = tree.totals(child).allocated;
            if bytes < MIN_FOLDER {
                continue;
            }
            folders.push((tree.path(index, child), bytes));
            stack.push(child);
        }
    }
    Snapshot {
        letter: index.letter,
        taken,
        used,
        folders,
    }
}

/// Find a folder by its path, walking down from the root.
pub fn find(index: &Index, tree: &Tree, path: &str) -> Option<NodeId> {
    let root = tree.path(index, tree.root());
    let rest = path
        .get(root.len()..)
        .filter(|_| path.len() >= root.len())?;
    let mut node = tree.root();
    for part in rest.split('\\').filter(|p| !p.is_empty()) {
        node = *tree
            .children(node)
            .iter()
            .find(|c| index.name(**c as usize).eq_ignore_ascii_case(part))?;
    }
    Some(node)
}

/// Where the space went between `old` and `new`, largest change first.
/// Growth and shrinkage are both reported, each where it concentrates;
/// changes smaller than `min_change` are left out.
pub fn diff(old: &Snapshot, new: &Snapshot, min_change: u64) -> Vec<Change> {
    let key = |p: &str| p.to_lowercase();
    let mut changes: HashMap<String, Change> = HashMap::new();
    for (path, bytes) in &old.folders {
        changes.insert(
            key(path),
            Change {
                path: path.clone(),
                before: *bytes,
                after: 0,
            },
        );
    }
    for (path, bytes) in &new.folders {
        changes
            .entry(key(path))
            .and_modify(|c| {
                c.after = *bytes;
                c.path = path.clone();
            })
            .or_insert(Change {
                path: path.clone(),
                before: 0,
                after: *bytes,
            });
    }

    // The largest growth and the largest shrinkage among each folder's
    // children, to tell where a change concentrates.
    let mut child_grew: HashMap<String, i64> = HashMap::new();
    let mut child_shrank: HashMap<String, i64> = HashMap::new();
    for (k, change) in &changes {
        if let Some(parent) = parent_key(k) {
            let d = change.delta();
            let up = child_grew.entry(parent.clone()).or_insert(0);
            *up = (*up).max(d);
            let down = child_shrank.entry(parent).or_insert(0);
            *down = (*down).min(d);
        }
    }

    let mut out: Vec<Change> = changes
        .into_iter()
        .filter(|(k, change)| {
            let d = change.delta();
            if d.unsigned_abs() < min_change {
                return false;
            }
            let child = if d > 0 {
                child_grew.get(k).copied().unwrap_or(0)
            } else {
                child_shrank.get(k).copied().unwrap_or(0)
            };
            (child as f64 / d as f64) < CONCENTRATION
        })
        .map(|(_, c)| c)
        .collect();
    out.sort_by(|a, b| {
        b.delta()
            .unsigned_abs()
            .cmp(&a.delta().unsigned_abs())
            .then(a.path.cmp(&b.path))
    });
    out
}

/// `c:\users\pc` → `c:\users`; `c:\users` → `c:\`; `c:\` → none.
fn parent_key(key: &str) -> Option<String> {
    let trimmed = key.trim_end_matches('\\');
    let cut = trimmed.rfind('\\')?;
    let parent = &trimmed[..cut];
    Some(if parent.ends_with(':') {
        format!("{parent}\\")
    } else {
        parent.to_string()
    })
}

/// A compact binary form: a header, then `(bytes, path)` per folder.
pub fn encode(snapshot: &Snapshot) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + snapshot.folders.len() * 64);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(snapshot.letter as u32).to_le_bytes());
    out.extend_from_slice(&snapshot.taken.to_le_bytes());
    out.extend_from_slice(&snapshot.used.to_le_bytes());
    out.extend_from_slice(&(snapshot.folders.len() as u32).to_le_bytes());
    for (path, bytes) in &snapshot.folders {
        out.extend_from_slice(&bytes.to_le_bytes());
        let raw = path.as_bytes();
        out.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        out.extend_from_slice(raw);
    }
    out
}

/// Just the header: `(letter, taken, used)`. Enough to list snapshots
/// without reading them whole.
pub fn decode_header(data: &[u8]) -> Option<(char, u64, u64)> {
    if data.len() < 32 || &data[..8] != MAGIC {
        return None;
    }
    let letter = char::from_u32(u32::from_le_bytes(data[8..12].try_into().ok()?))?;
    let taken = u64::from_le_bytes(data[12..20].try_into().ok()?);
    let used = u64::from_le_bytes(data[20..28].try_into().ok()?);
    Some((letter, taken, used))
}

/// Read a snapshot back. `None` for anything damaged or foreign.
pub fn decode(data: &[u8]) -> Option<Snapshot> {
    let (letter, taken, used) = decode_header(data)?;
    let count = u32::from_le_bytes(data[28..32].try_into().ok()?) as usize;
    let mut folders = Vec::with_capacity(count.min(1 << 20));
    let mut pos = 32;
    for _ in 0..count {
        let bytes = u64::from_le_bytes(data.get(pos..pos + 8)?.try_into().ok()?);
        let len = u16::from_le_bytes(data.get(pos + 8..pos + 10)?.try_into().ok()?) as usize;
        let path = std::str::from_utf8(data.get(pos + 10..pos + 10 + len)?).ok()?;
        folders.push((path.to_string(), bytes));
        pos += 10 + len;
    }
    Some(Snapshot {
        letter,
        taken,
        used,
        folders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferret_core::testing::{dir, file, index_from_specs};
    use ferret_core::ROOT_RECORD;

    fn snap(folders: &[(&str, u64)]) -> Snapshot {
        Snapshot {
            letter: 'C',
            taken: 1,
            used: 0,
            folders: folders.iter().map(|(p, b)| (p.to_string(), *b)).collect(),
        }
    }

    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;

    #[test]
    fn growth_is_reported_where_it_happened() {
        let old = snap(&[
            (r"C:\", 100 * GB),
            (r"C:\Users", 50 * GB),
            (r"C:\Users\pc", 50 * GB),
            (r"C:\Users\pc\Docker", 5 * GB),
            (r"C:\Users\pc\Videos", 20 * GB),
        ]);
        let new = snap(&[
            (r"C:\", 110 * GB),
            (r"C:\Users", 60 * GB),
            (r"C:\Users\pc", 60 * GB),
            (r"C:\Users\pc\Docker", 15 * GB),
            (r"C:\Users\pc\Videos", 20 * GB),
        ]);
        let changes = diff(&old, &new, 10 * MB);
        let paths: Vec<_> = changes.iter().map(|c| c.path.as_str()).collect();
        // Not C:\, not Users, not pc: Docker is where the 10 GB went.
        assert_eq!(paths, [r"C:\Users\pc\Docker"]);
        assert_eq!(changes[0].delta(), 10 * GB as i64);
    }

    #[test]
    fn spread_growth_keeps_the_parent() {
        let old = snap(&[(r"C:\", 10 * GB), (r"C:\a", GB), (r"C:\b", GB)]);
        let new = snap(&[(r"C:\", 12 * GB), (r"C:\a", 2 * GB), (r"C:\b", 2 * GB)]);
        let paths: Vec<_> = diff(&old, &new, MB).into_iter().map(|c| c.path).collect();
        assert!(paths.contains(&r"C:\".to_string()));
        assert!(paths.contains(&r"C:\a".to_string()));
        assert!(paths.contains(&r"C:\b".to_string()));
    }

    #[test]
    fn a_new_folder_and_a_deleted_one_both_show() {
        let old = snap(&[(r"C:\", 10 * GB), (r"C:\gone", 3 * GB)]);
        let new = snap(&[(r"C:\", 10 * GB), (r"C:\fresh", 3 * GB)]);
        let changes = diff(&old, &new, MB);
        let find = |p: &str| changes.iter().find(|c| c.path == p).map(|c| c.delta());
        assert_eq!(find(r"C:\fresh"), Some(3 * GB as i64));
        assert_eq!(find(r"C:\gone"), Some(-(3 * GB as i64)));
        // The root did not change at all.
        assert_eq!(find(r"C:\"), None);
    }

    #[test]
    fn paths_match_regardless_of_case() {
        let old = snap(&[(r"C:\", GB), (r"C:\Users", GB)]);
        let new = snap(&[(r"C:\", GB), (r"C:\USERS", GB)]);
        assert!(diff(&old, &new, 1).is_empty());
    }

    #[test]
    fn a_snapshot_survives_the_round_trip() {
        let s = Snapshot {
            letter: 'D',
            taken: 1_750_000_000,
            used: 123 * GB,
            folders: vec![(r"D:\".into(), 5 * GB), (r"D:\Müzik".into(), 3 * GB)],
        };
        let bytes = encode(&s);
        assert_eq!(decode_header(&bytes), Some(('D', 1_750_000_000, 123 * GB)));
        assert_eq!(decode(&bytes), Some(s));
        assert!(decode(&bytes[..bytes.len() - 3]).is_none());
        assert!(decode(b"not a snapshot at all, no no no").is_none());
    }

    #[test]
    fn capture_records_big_folders_and_find_walks_back_to_them() {
        let index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            dir(21, 20, "pc"),
            file(22, 21, "big.bin").sized(5 * MB),
            dir(23, 20, "tiny"),
            file(24, 23, "small.txt").sized(1000),
        ]);
        let tree = Tree::build(&index);
        let s = capture(&index, &tree, 9 * GB, 42);
        let paths: Vec<_> = s.folders.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, [r"C:\", r"C:\Users", r"C:\Users\pc"]);
        assert_eq!(find(&index, &tree, r"C:\users\PC"), Some(1));
        assert_eq!(find(&index, &tree, r"C:\"), Some(tree.root()));
        assert_eq!(find(&index, &tree, r"C:\nope"), None);
    }

    #[test]
    fn parents_of_paths() {
        assert_eq!(parent_key(r"c:\users\pc").as_deref(), Some(r"c:\users"));
        assert_eq!(parent_key(r"c:\users").as_deref(), Some(r"c:\"));
        assert_eq!(parent_key(r"c:\"), None);
    }
}
