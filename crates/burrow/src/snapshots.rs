//! Where snapshots live: `%LOCALAPPDATA%\Burrow\snapshots`, one small
//! file per scan, named after the drive and the time.
//!
//! Twelve are kept per drive. Scanning again within an hour of the last
//! snapshot saves nothing: that hour's first record stays the baseline, an
//! afternoon of rescans does not push last month's snapshot out, and a
//! snapshot being compared against is never replaced under the comparison.

use std::path::PathBuf;

use burrow_tree::snapshot::{self, Snapshot};

const KEEP: usize = 12;
const SKIP_WITHIN: u64 = 3600;

/// A saved snapshot, known by its header.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub taken: u64,
    pub used: u64,
}

fn folder() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("Burrow").join("snapshots"))
}

/// Snapshots of a drive, newest first.
pub fn list(letter: char) -> Vec<Saved> {
    let Some(dir) = folder() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let prefix = format!("{}-", letter.to_ascii_uppercase());
    let mut out: Vec<Saved> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".snap"))
        })
        .filter_map(|path| {
            // The header is at the front; no need to read a whole file.
            use std::io::Read;
            let mut head = [0u8; 32];
            std::fs::File::open(&path)
                .ok()?
                .read_exact(&mut head)
                .ok()?;
            let (l, taken, used) = snapshot::decode_header(&head)?;
            (l == letter.to_ascii_uppercase()).then_some(Saved { path, taken, used })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.taken));
    out
}

pub fn load(saved: &Saved) -> Option<Snapshot> {
    snapshot::decode(&std::fs::read(&saved.path).ok()?)
}

/// Save unless there is already one from the last hour; prune to [`KEEP`].
pub fn save(snap: &Snapshot) -> std::io::Result<()> {
    let dir = folder().ok_or_else(|| std::io::Error::other("no LOCALAPPDATA"))?;
    std::fs::create_dir_all(&dir)?;
    let existing = list(snap.letter);
    if let Some(recent) = existing.first() {
        if snap.taken.saturating_sub(recent.taken) < SKIP_WITHIN {
            return Ok(());
        }
    }
    let name = format!("{}-{}.snap", snap.letter.to_ascii_uppercase(), snap.taken);
    std::fs::write(dir.join(name), snapshot::encode(snap))?;
    for old in list(snap.letter).into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(old.path);
    }
    Ok(())
}
