//! Full `$MFT` scan: walk every record on a volume and build a file index.
//!
//! The point of reading the MFT directly is that it is one sequential pass over
//! a table, instead of millions of directory-tree syscalls. Walking `C:\` with
//! the normal Windows APIs takes minutes; this takes seconds.
//!
//! `$MFT` is itself a file, so the scan bootstraps: read record 0, decode its
//! `$DATA` run list, and that gives the location of every other record.
//!
//! # Memory layout
//!
//! A 2 M record volume means the per-entry cost is what decides whether the app
//! sits at 150 MB or 400 MB. So [`Entry`] is a flat 32-byte struct and names
//! live in one shared arena: a `Box<str>` per entry would add two million heap
//! allocations, their allocator headers, and a pointer chase per comparison.

use std::io;
use std::time::{Duration, Instant};

use crate::record::{
    self, ATTR_ATTRIBUTE_LIST, ATTR_DATA, ATTR_FILE_NAME, ATTR_INDEX_ALLOCATION, ATTR_STANDARD_INFO,
};
use crate::runs::{parse_runs, Run};
use crate::volume::Volume;

/// MFT record number of the root directory. Its parent is itself.
pub const ROOT_RECORD: u32 = 5;

/// Records 0..16 are NTFS's own metafiles: `$MFT`, `$LogFile`, `$Bitmap` and
/// friends. They are real files, but nobody searching their disk means them.
const FIRST_USER_RECORD: u32 = 16;

/// Read this much of the MFT per volume read. Large enough that the syscall
/// overhead disappears, small enough to stay friendly to memory.
const CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// Marker for "no entry" in the record -> entry lookup table.
const NO_ENTRY: u32 = u32::MAX;

/// Entry flags.
pub const IS_DIR: u16 = 1 << 0;
pub const IS_HIDDEN: u16 = 1 << 1;
pub const IS_SYSTEM: u16 = 1 << 2;
pub const IS_READONLY: u16 = 1 << 3;
/// Set when the change journal reported the file gone. The entry stays in place
/// so that positions — and therefore the prebuilt search arena — remain valid;
/// queries filter it out.
pub const IS_DELETED: u16 = 1 << 4;
/// The file has more than one name (hard links). It is indexed — and its
/// clusters counted — once, under the first name found.
pub const IS_HARDLINK: u16 = 1 << 5;
pub const IS_COMPRESSED: u16 = 1 << 6;
pub const IS_SPARSE: u16 = 1 << 7;
/// A reparse point: symlink, junction, deduplicated file, cloud placeholder.
pub const IS_REPARSE: u16 = 1 << 8;
/// The content is not stored locally (OneDrive "online-only" and similar).
pub const IS_CLOUD: u16 = 1 << 9;

/// One indexed file or directory.
///
/// The name is not stored inline; ask the owning [`Index`] for it with
/// [`Index::name`].
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub record: u32,
    /// Record number of the containing directory.
    pub parent: u32,
    /// Logical size of the unnamed data stream: what Explorer calls "Size".
    pub size: u64,
    /// Clusters actually occupied, in bytes: "Size on disk". Includes
    /// alternate data streams and, for a directory, its index. Zero for a
    /// file small enough to live inside its own MFT record.
    pub allocated: u64,
    /// Last modification, as a Windows FILETIME (100 ns ticks since 1601).
    pub modified: u64,
    name_offset: u32,
    name_len: u16,
    pub flags: u16,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.flags & IS_DIR != 0
    }
    pub fn is_hidden(&self) -> bool {
        self.flags & IS_HIDDEN != 0
    }
    pub fn is_system(&self) -> bool {
        self.flags & IS_SYSTEM != 0
    }
    pub fn is_deleted(&self) -> bool {
        self.flags & IS_DELETED != 0
    }
    pub fn is_hardlink(&self) -> bool {
        self.flags & IS_HARDLINK != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.flags & IS_REPARSE != 0
    }
    pub fn is_cloud(&self) -> bool {
        self.flags & IS_CLOUD != 0
    }
}

/// Current size information for one file, fetched through the normal
/// filesystem by whoever applies a change-journal entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub allocated: u64,
    pub modified: u64,
}

/// Convert a Windows FILETIME to Unix epoch seconds.
///
/// Returns `None` for the zero value, which means "never set".
pub fn filetime_to_unix(filetime: u64) -> Option<i64> {
    if filetime == 0 {
        return None;
    }
    // FILETIME counts 100 ns ticks from 1601-01-01; Unix counts seconds from
    // 1970-01-01. 11644473600 seconds separate the two epochs.
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const EPOCH_DIFFERENCE: i64 = 11_644_473_600;
    Some((filetime / TICKS_PER_SECOND) as i64 - EPOCH_DIFFERENCE)
}

/// Knobs for [`scan_with`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanOptions {
    /// Include NTFS's internal metafiles (`$MFT`, `$LogFile`, …).
    /// Off by default: nobody searching their disk means those.
    pub include_system: bool,
}

/// What a scan found, for reporting and benchmarking.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanStats {
    pub records_total: u64,
    pub records_in_use: u64,
    pub files: u64,
    pub dirs: u64,
    /// Records skipped because they failed the fixup or header checks.
    pub records_damaged: u64,
    /// Metafiles left out because `include_system` was false.
    pub records_system: u64,
    /// Entries dropped because no path back to the root could be built.
    pub records_orphaned: u64,
    /// Files whose attributes spilled into extension records and had to be
    /// stitched back together after the pass.
    pub records_stitched: u64,
    pub mft_bytes: u64,
    pub read_time: Duration,
    pub total_time: Duration,
}

/// What [`Index::apply_change`] did.
#[derive(Debug, Default, Clone, Copy)]
pub struct Refresh {
    /// The entry is different from what it was.
    pub changed: bool,
    /// A name was added or replaced, so the search arena is now stale.
    pub names_changed: bool,
}

/// An in-memory index of one volume.
///
/// Entries are stored densely — a 2 M record MFT is typically 10 % free space,
/// and the dense layout is what makes the search pass cache-friendly. Sparse
/// record numbers are mapped back through [`Index::by_record`].
pub struct Index {
    pub letter: char,
    entries: Vec<Entry>,
    /// Every name, concatenated. Entries hold offsets into this.
    names: String,
    /// `record number -> position in entries`, or [`NO_ENTRY`].
    by_record: Vec<u32>,
    pub stats: ScanStats,
}

impl Index {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Name of the entry at `position`.
    pub fn name(&self, position: usize) -> &str {
        match self.entries.get(position) {
            Some(entry) => self.name_of(entry),
            None => "",
        }
    }

    /// Name of a specific entry (which must belong to this index).
    pub fn name_of(&self, entry: &Entry) -> &str {
        let start = entry.name_offset as usize;
        let end = start + entry.name_len as usize;
        self.names.get(start..end).unwrap_or("")
    }

    /// Look up an entry by its MFT record number.
    pub fn by_record(&self, record: u32) -> Option<(usize, &Entry)> {
        let slot = *self.by_record.get(record as usize)?;
        if slot == NO_ENTRY {
            return None;
        }
        let position = slot as usize;
        Some((position, self.entries.get(position)?))
    }

    /// Bytes held by the index itself, for reporting memory use.
    pub fn memory_bytes(&self) -> usize {
        self.entries.len() * std::mem::size_of::<Entry>()
            + self.names.len()
            + self.by_record.len() * std::mem::size_of::<u32>()
    }

    /// Build the full path of the entry at `position`, e.g.
    /// `C:\Users\pc\notes.txt`.
    ///
    /// Returns `None` when the chain to the root is broken — which happens for
    /// entries whose parent directory was skipped or is damaged.
    pub fn full_path(&self, position: usize) -> Option<String> {
        let mut path = String::with_capacity(96);
        self.write_full_path(position, &mut path).then_some(path)
    }

    /// Same as [`Index::full_path`] but appends into an existing buffer, so a
    /// caller rendering thousands of rows can reuse one allocation.
    pub fn write_full_path(&self, position: usize, out: &mut String) -> bool {
        let Some(entry) = self.entries.get(position) else {
            return false;
        };

        // Collect the chain first: paths are built root-first but walked
        // leaf-first. Sixteen levels covers essentially every real path
        // without spilling to the heap for the common case.
        let mut chain: Vec<&str> = Vec::with_capacity(16);
        chain.push(self.name_of(entry));
        let mut current = entry.parent;

        // The root is its own parent, so the walk needs an explicit stop, and a
        // depth cap in case a damaged volume produces a cycle.
        for _ in 0..256 {
            if current == ROOT_RECORD {
                break;
            }
            let Some((_, parent)) = self.by_record(current) else {
                return false;
            };
            chain.push(self.name_of(parent));
            if parent.parent == current {
                break;
            }
            current = parent.parent;
        }

        out.clear();
        out.push(self.letter);
        out.push(':');
        for part in chain.iter().rev() {
            out.push('\\');
            out.push_str(part);
        }
        true
    }

    /// Fold one change-journal entry into the index.
    ///
    /// Everything but the size comes from the journal entry itself. That is not
    /// a shortcut — it is the only correct source. Ferret reads volumes raw,
    /// which bypasses the filesystem cache, so the MFT record of a file created
    /// a second ago is still blank on disk; re-reading it would lose almost
    /// every change. Sizes and time are supplied by the caller, which can
    /// stat the path through the normal filesystem and see current data.
    pub fn apply_change(&mut self, change: &crate::journal::Change, stat: Option<FileStat>) -> Refresh {
        let names_before = self.names.len();

        let changed = if change.is_delete() {
            self.mark_deleted(change.record)
        } else {
            let mut flags = 0u16;
            if change.is_dir() {
                flags |= IS_DIR;
            }
            if change.is_hidden() {
                flags |= IS_HIDDEN;
            }
            if change.is_system() {
                flags |= IS_SYSTEM;
            }

            // Without a stat, keep whatever size and time the entry already has
            // rather than reporting a file as empty.
            let previous = self.position_of(change.record).map(|p| self.entries[p]);
            if let Some(entry) = previous {
                // The journal does not know about link counts; keep what the
                // scan found.
                flags |= entry.flags & IS_HARDLINK;
            }
            let fallback = match previous {
                Some(entry) => FileStat {
                    size: entry.size,
                    allocated: entry.allocated,
                    modified: entry.modified,
                },
                None => FileStat::default(),
            };
            let stat = stat.unwrap_or(fallback);

            self.upsert(
                change.record,
                ParsedEntry {
                    parent: change.parent,
                    name: change.name.clone(),
                    size: stat.size,
                    allocated: stat.allocated,
                    modified: stat.modified,
                    flags,
                },
            )
        };

        Refresh {
            changed,
            names_changed: self.names.len() != names_before,
        }
    }

    /// Insert or update one entry. Returns whether anything changed.
    /// Keep the file and directory counts true as entries come and go.
    fn count(&mut self, is_dir: bool, delta: i64) {
        let counter = if is_dir {
            &mut self.stats.dirs
        } else {
            &mut self.stats.files
        };
        *counter = counter.saturating_add_signed(delta);
    }

    /// Insert or update one entry. Returns whether anything changed.
    fn upsert(&mut self, record: u32, parsed: ParsedEntry) -> bool {
        let mut flags = parsed.flags;
        // An entry can come back after having been deleted, if the record was
        // reused for a new file.
        flags &= !IS_DELETED;

        if let Some(position) = self.position_of(record) {
            let existing = self.entries[position];
            let same_name = self.name(position) == parsed.name;

            if same_name
                && existing.parent == parsed.parent
                && existing.size == parsed.size
                && existing.allocated == parsed.allocated
                && existing.modified == parsed.modified
                && existing.flags == flags
            {
                return false;
            }

            // Names are append-only: rewriting the arena in place would shift
            // every later offset. A rename leaks its old bytes until the next
            // full scan, which is a fair trade for O(1) updates.
            let (name_offset, name_len) = if same_name {
                (existing.name_offset, existing.name_len)
            } else {
                self.push_name(&parsed.name)
            };

            // A record can come back as a different kind of thing, or return
            // from the dead when NTFS reuses its number.
            if existing.is_deleted() {
                self.count(flags & IS_DIR != 0, 1);
            } else if existing.is_dir() != (flags & IS_DIR != 0) {
                self.count(existing.is_dir(), -1);
                self.count(flags & IS_DIR != 0, 1);
            }

            self.entries[position] = Entry {
                record,
                parent: parsed.parent,
                size: parsed.size,
                allocated: parsed.allocated,
                modified: parsed.modified,
                name_offset,
                name_len,
                flags,
            };
            return true;
        }

        let (name_offset, name_len) = self.push_name(&parsed.name);
        let position = self.entries.len() as u32;
        self.entries.push(Entry {
            record,
            parent: parsed.parent,
            size: parsed.size,
            allocated: parsed.allocated,
            modified: parsed.modified,
            name_offset,
            name_len,
            flags,
        });

        if record as usize >= self.by_record.len() {
            self.by_record.resize(record as usize + 1, NO_ENTRY);
        }
        self.by_record[record as usize] = position;
        self.count(flags & IS_DIR != 0, 1);
        true
    }

    /// Flag an entry as gone. Returns whether it was there to begin with.
    fn mark_deleted(&mut self, record: u32) -> bool {
        let Some(position) = self.position_of(record) else {
            return false;
        };
        if self.entries[position].flags & IS_DELETED != 0 {
            return false;
        }
        self.entries[position].flags |= IS_DELETED;
        let was_dir = self.entries[position].is_dir();
        self.count(was_dir, -1);
        true
    }

    fn position_of(&self, record: u32) -> Option<usize> {
        let slot = *self.by_record.get(record as usize)?;
        (slot != NO_ENTRY).then_some(slot as usize)
    }

    fn push_name(&mut self, name: &str) -> (u32, u16) {
        let offset = self.names.len() as u32;
        self.names.push_str(name);
        (offset, name.len() as u16)
    }

    /// Directory that contains the entry at `position`, without the file name.
    pub fn parent_path(&self, position: usize) -> Option<String> {
        let entry = self.entries.get(position)?;
        if entry.parent == ROOT_RECORD {
            return Some(format!("{}:\\", self.letter));
        }
        let (parent_position, _) = self.by_record(entry.parent)?;
        self.full_path(parent_position)
    }
}

/// How far a scan has got, for a progress bar.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub records_done: u64,
    /// Estimated from the size of `$MFT`; the table can grow mid-scan, so treat
    /// this as an upper bound rather than a promise.
    pub records_total: u64,
}

impl Progress {
    /// 0.0 to 1.0, clamped — a growing table must not report 130 %.
    pub fn fraction(&self) -> f64 {
        if self.records_total == 0 {
            return 0.0;
        }
        (self.records_done as f64 / self.records_total as f64).clamp(0.0, 1.0)
    }
}

/// Scan a volume with the default options.
pub fn scan(letter: char) -> io::Result<Index> {
    scan_with(letter, ScanOptions::default())
}

/// Scan a whole volume and return its index.
pub fn scan_with(letter: char, options: ScanOptions) -> io::Result<Index> {
    scan_with_progress(letter, options, |_| {})
}

/// Scan a whole volume, reporting progress as it goes.
///
/// `on_progress` is called once per read chunk — a few dozen times for a whole
/// disk — so a caller that wants to throttle further can, and one that does not
/// will not drown.
pub fn scan_with_progress(
    letter: char,
    options: ScanOptions,
    mut on_progress: impl FnMut(Progress),
) -> io::Result<Index> {
    let started = Instant::now();
    let mut volume = Volume::open(letter)?;

    let runs = read_mft_runs(&mut volume)?;
    let mft_bytes = crate::runs::total_clusters(&runs) * volume.bytes_per_cluster;
    let record_size = volume.bytes_per_record as usize;
    if record_size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the volume reports a zero-byte MFT record size",
        ));
    }

    let estimated = (mft_bytes / record_size as u64) as usize;
    let mut entries: Vec<Entry> = Vec::with_capacity(estimated * 9 / 10);
    let mut names = String::with_capacity(estimated * 20);
    let mut by_record: Vec<u32> = vec![NO_ENTRY; estimated];

    let mut stats = ScanStats {
        mft_bytes,
        ..Default::default()
    };
    let sector = volume.bytes_per_sector as usize;

    // Round the chunk down to a whole number of records so a record never
    // straddles two chunks.
    let chunk_bytes = (CHUNK_BYTES - CHUNK_BYTES % record_size as u64).max(record_size as u64);
    let mut buffer = vec![0u8; chunk_bytes as usize];

    let mut record_number: u64 = 0;

    // Files whose attributes overflowed into extension records. Their size
    // cannot be known until every piece has been read, and an extension can
    // sit at a lower record number than its base, so both halves wait for
    // the end of the pass. On a typical system disk this is a few thousand
    // records — but they are the big, fragmented files a disk-usage view
    // exists to find.
    let mut partial: std::collections::HashMap<u32, Parts> = std::collections::HashMap::new();
    let mut extensions: Vec<(u32, Parts)> = Vec::new();

    for run in &runs {
        let Some(lcn) = run.lcn else {
            // A sparse region inside $MFT: no records stored there.
            record_number += run.clusters * volume.bytes_per_cluster / record_size as u64;
            continue;
        };

        let run_bytes = run.clusters * volume.bytes_per_cluster;
        let mut consumed = 0u64;

        while consumed < run_bytes {
            let want = chunk_bytes.min(run_bytes - consumed);
            let slice = &mut buffer[..want as usize];

            let read_started = Instant::now();
            volume.read_at(lcn * volume.bytes_per_cluster + consumed, slice)?;
            stats.read_time += read_started.elapsed();

            on_progress(Progress {
                records_done: record_number,
                records_total: estimated as u64,
            });

            for raw in slice.chunks_mut(record_size) {
                let number = record_number as u32;
                record_number += 1;
                stats.records_total += 1;

                match parse_record(raw, sector) {
                    ParseOutcome::Complete(parts) => {
                        stats.records_in_use += 1;
                        if let Some(parsed) = parts.finish() {
                            let sink = Sink {
                                entries: &mut entries,
                                names: &mut names,
                                by_record: &mut by_record,
                            };
                            sink.push(number, parsed, options, &mut stats);
                        }
                    }
                    ParseOutcome::Partial(parts) => {
                        stats.records_in_use += 1;
                        partial.insert(number, parts);
                    }
                    ParseOutcome::Extension { base, parts } => {
                        extensions.push((base, parts));
                    }
                    ParseOutcome::Free => {}
                    ParseOutcome::Damaged => stats.records_damaged += 1,
                }
            }

            consumed += want;
        }
    }

    // Stitch overflowed files back together. An extension whose base never
    // turned up belongs to a file deleted mid-scan; it is simply dropped.
    for (base, parts) in extensions {
        if let Some(owner) = partial.get_mut(&base) {
            owner.absorb(parts);
        }
    }
    let mut stitched: Vec<(u32, Parts)> = partial.into_iter().collect();
    stitched.sort_unstable_by_key(|(number, _)| *number);
    stats.records_stitched = stitched.len() as u64;
    for (number, parts) in stitched {
        if let Some(parsed) = parts.finish() {
            let sink = Sink {
                entries: &mut entries,
                names: &mut names,
                by_record: &mut by_record,
            };
            sink.push(number, parsed, options, &mut stats);
        }
    }

    // Drop anything that cannot be traced back to the root directory.
    //
    // Two kinds of record end up here. Metafiles nest — `$Extend` holds
    // `$Quota`, `$ObjId`, `$RmMetadata` and its transaction logs — and dropping
    // the parent orphans the children. Record numbers are recycled, so a child
    // can sit at a *lower* number than its parent and no forward pass can catch
    // it; reachability has to be decided once the whole table is in memory. The
    // rest are genuinely damaged chains. Neither can be given a path, so
    // neither belongs in a file index.
    let removed = retain_reachable(&mut entries, &mut names, &mut by_record);
    stats.records_orphaned = removed;
    stats.files = entries.iter().filter(|e| !e.is_dir()).count() as u64;
    stats.dirs = entries.iter().filter(|e| e.is_dir()).count() as u64;

    entries.shrink_to_fit();
    names.shrink_to_fit();
    stats.total_time = started.elapsed();

    Ok(Index {
        letter: volume.letter,
        entries,
        names,
        by_record,
        stats,
    })
}

/// Reachability of one entry, memoised while walking parent chains.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Unknown,
    Yes,
    No,
}

/// Remove every entry whose parent chain does not end at the root, rebuilding
/// the name arena and the record lookup around the survivors.
///
/// Returns how many entries were dropped.
fn retain_reachable(entries: &mut Vec<Entry>, names: &mut String, by_record: &mut [u32]) -> u64 {
    let mut state = vec![Reach::Unknown; entries.len()];
    // Reused across entries so a deep directory chain is walked once, not once
    // per file inside it.
    let mut pending: Vec<usize> = Vec::with_capacity(32);

    let lookup = |record: u32, by_record: &[u32]| -> Option<usize> {
        let slot = *by_record.get(record as usize)?;
        (slot != NO_ENTRY).then_some(slot as usize)
    };

    for start in 0..entries.len() {
        if state[start] != Reach::Unknown {
            continue;
        }
        pending.clear();
        let mut current = start;
        let verdict;

        loop {
            let parent = entries[current].parent;
            if parent == ROOT_RECORD || parent == entries[current].record {
                verdict = Reach::Yes;
                break;
            }
            let Some(next) = lookup(parent, by_record) else {
                verdict = Reach::No;
                break;
            };
            match state[next] {
                Reach::Yes => {
                    verdict = Reach::Yes;
                    break;
                }
                Reach::No => {
                    verdict = Reach::No;
                    break;
                }
                Reach::Unknown => {}
            }
            // Mark as in-progress by pushing; a cycle would revisit a node
            // already on the stack, which the depth cap below catches.
            pending.push(current);
            if pending.len() > 256 {
                verdict = Reach::No;
                break;
            }
            current = next;
        }

        state[current] = verdict;
        for node in pending.iter() {
            state[*node] = verdict;
        }
    }

    let kept: Vec<usize> = (0..entries.len())
        .filter(|i| state[*i] == Reach::Yes)
        .collect();
    let removed = (entries.len() - kept.len()) as u64;
    if removed == 0 {
        return 0;
    }

    let mut new_names = String::with_capacity(names.len());
    let mut new_entries = Vec::with_capacity(kept.len());
    for old in kept {
        let mut entry = entries[old];
        let start = entry.name_offset as usize;
        let end = start + entry.name_len as usize;
        let name = names.get(start..end).unwrap_or("");
        entry.name_offset = new_names.len() as u32;
        new_names.push_str(name);
        new_entries.push(entry);
    }

    by_record.iter_mut().for_each(|slot| *slot = NO_ENTRY);
    for (position, entry) in new_entries.iter().enumerate() {
        if let Some(slot) = by_record.get_mut(entry.record as usize) {
            *slot = position as u32;
        }
    }

    *entries = new_entries;
    *names = new_names;
    removed
}

/// NTFS metafiles: the low records, plus the `$…` entries sitting in the root.
fn is_system_record(number: u32, parsed: &ParsedEntry) -> bool {
    number < FIRST_USER_RECORD || (parsed.parent == ROOT_RECORD && parsed.name.starts_with('$'))
}

/// A record decoded but not yet placed into the index's arenas.
struct ParsedEntry {
    parent: u32,
    name: String,
    size: u64,
    allocated: u64,
    modified: u64,
    flags: u16,
}

/// Where finished entries go during a scan: the three arenas an [`Index`] is
/// built from.
struct Sink<'a> {
    entries: &'a mut Vec<Entry>,
    names: &'a mut String,
    by_record: &'a mut Vec<u32>,
}

impl Sink<'_> {
    fn push(self, number: u32, parsed: ParsedEntry, options: ScanOptions, stats: &mut ScanStats) {
        if !options.include_system && is_system_record(number, &parsed) {
            stats.records_system += 1;
            return;
        }
        if number as usize >= self.by_record.len() {
            // The MFT grew past the estimate mid-scan.
            self.by_record.resize(number as usize + 1, NO_ENTRY);
        }
        self.by_record[number as usize] = self.entries.len() as u32;

        let name_offset = self.names.len() as u32;
        self.names.push_str(&parsed.name);
        self.entries.push(Entry {
            record: number,
            parent: parsed.parent,
            size: parsed.size,
            allocated: parsed.allocated,
            modified: parsed.modified,
            name_offset,
            name_len: parsed.name.len() as u16,
            flags: parsed.flags,
        });
    }
}

/// What one or more records say about a file, before it is finished.
///
/// A file normally fits in one record. When it does not, its attributes are
/// scattered across a base record and extension records, and each contributes
/// a [`Parts`] that [`Parts::absorb`] merges.
#[derive(Default)]
struct Parts {
    is_dir: bool,
    name: Option<record::FileName>,
    /// Human-readable names seen: more than one means hard links.
    links: u32,
    info: Option<record::StandardInfo>,
    /// `(logical, on disk)` of the unnamed data stream, from its first piece.
    data: Option<(u64, u64)>,
    /// Clusters held by everything else: alternate data streams and a
    /// directory's index.
    other_on_disk: u64,
}

impl Parts {
    fn absorb(&mut self, other: Parts) {
        self.is_dir |= other.is_dir;
        if let Some(name) = other.name {
            self.name = Some(choose_name(self.name.take(), name));
        }
        self.links += other.links;
        if self.info.is_none() {
            self.info = other.info;
        }
        if self.data.is_none() {
            self.data = other.data;
        }
        self.other_on_disk += other.other_on_disk;
    }

    /// Turn the collected pieces into an entry, or `None` for a record with
    /// no name — things like `$MFT`'s own extents.
    fn finish(self) -> Option<ParsedEntry> {
        let name = self.name?;
        // A name longer than the arena's length field could not be addressed.
        if name.name.len() > u16::MAX as usize {
            return None;
        }

        let mut flags = 0u16;
        if self.is_dir {
            flags |= IS_DIR;
        }
        if let Some(info) = &self.info {
            flags |= info.flags();
        }
        if self.links > 1 {
            flags |= IS_HARDLINK;
        }

        // `$FILE_NAME` sizes are only refreshed when the directory entry is,
        // so they are the fallback, never the first choice.
        let (size, data_on_disk) = self
            .data
            .unwrap_or((name.real_size, name.allocated_size));

        Some(ParsedEntry {
            parent: name.parent as u32,
            name: name.name,
            size,
            allocated: data_on_disk + self.other_on_disk,
            modified: self.info.map(|i| i.modified).unwrap_or(0),
            flags,
        })
    }
}

enum ParseOutcome {
    /// A file described entirely by this one record.
    Complete(Parts),
    /// A base record with an attribute list: more pieces live elsewhere.
    Partial(Parts),
    /// A piece of the file whose base record is `base`.
    Extension { base: u32, parts: Parts },
    /// A record that is not in use, or one Ferret deliberately skips.
    Free,
    Damaged,
}

/// Decode one raw record.
fn parse_record(raw: &mut [u8], bytes_per_sector: usize) -> ParseOutcome {
    // An all-zero record is simply unused space in the MFT, not damage.
    if raw.iter().take(4).all(|b| *b == 0) {
        return ParseOutcome::Free;
    }
    if !record::apply_fixups(raw, bytes_per_sector) {
        return ParseOutcome::Damaged;
    }
    let Some(header) = record::parse_header(raw) else {
        return ParseOutcome::Damaged;
    };
    if !header.in_use() {
        return ParseOutcome::Free;
    }

    let mut parts = Parts {
        is_dir: header.is_directory(),
        ..Parts::default()
    };
    let mut has_list = false;

    for attr in record::attributes(raw, &header) {
        match attr.kind {
            ATTR_STANDARD_INFO => {
                if let Some(value) = attr.resident_value() {
                    parts.info = record::parse_standard_info(value);
                }
            }
            ATTR_ATTRIBUTE_LIST => has_list = true,
            ATTR_FILE_NAME => {
                if let Some(value) = attr.resident_value() {
                    if let Some(parsed) = record::parse_file_name(value) {
                        if parsed.namespace.is_preferred() {
                            parts.links += 1;
                        }
                        parts.name = Some(choose_name(parts.name.take(), parsed));
                    }
                }
            }
            // A large attribute is split into pieces across records; only the
            // first carries its sizes, the rest must not be counted again.
            ATTR_DATA if attr.is_first_piece() => {
                if attr.name_len == 0 {
                    parts.data = Some((attr.content_size(), attr.on_disk_size()));
                } else {
                    // Alternate data streams hold real clusters too.
                    parts.other_on_disk += attr.on_disk_size();
                }
            }
            ATTR_INDEX_ALLOCATION if attr.is_first_piece() => {
                parts.other_on_disk += attr.on_disk_size();
            }
            _ => {}
        }
    }

    if header.is_extension() {
        return ParseOutcome::Extension {
            base: header.base_record as u32,
            parts,
        };
    }
    if has_list {
        ParseOutcome::Partial(parts)
    } else {
        ParseOutcome::Complete(parts)
    }
}

/// Prefer the human-readable name over the 8.3 alias.
fn choose_name(current: Option<record::FileName>, candidate: record::FileName) -> record::FileName {
    match current {
        None => candidate,
        Some(existing) => {
            if !existing.namespace.is_preferred() && candidate.namespace.is_preferred() {
                candidate
            } else {
                existing
            }
        }
    }
}

/// Read record 0 (`$MFT` itself) and decode the run list of its `$DATA`.
fn read_mft_runs(volume: &mut Volume) -> io::Result<Vec<Run>> {
    let record_size = volume.bytes_per_record as usize;
    let mut raw = vec![0u8; record_size];
    let offset = volume.mft_start_lcn * volume.bytes_per_cluster;
    volume.read_at(offset, &mut raw)?;

    let sector = volume.bytes_per_sector as usize;
    if !record::apply_fixups(&mut raw, sector) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "could not read $MFT record 0: bad signature or fixups",
        ));
    }
    let header = record::parse_header(&raw).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the $MFT record header is damaged",
        )
    })?;

    for attr in record::attributes(&raw, &header) {
        if attr.kind == ATTR_DATA && attr.name_len == 0 {
            if let Some(list) = attr.run_list() {
                let runs = parse_runs(list);
                if !runs.is_empty() {
                    return Ok(runs);
                }
                break;
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no $DATA run list found in $MFT",
    ))
}

/// Index construction helpers for tests.
///
/// Public behind the `testing` feature so the application crate can exercise
/// its own row building and formatting against a known index, instead of
/// needing a real disk and administrator rights to test a string.
#[cfg(any(test, feature = "testing"))]
pub mod test_support {
    use super::*;

    pub struct Spec {
        pub record: u32,
        pub parent: u32,
        pub name: &'static str,
        pub flags: u16,
        pub size: u64,
        pub allocated: u64,
        pub modified: u64,
    }

    impl Spec {
        /// Give the entry a size; on-disk size is rounded up to 4 KB clusters.
        pub fn sized(mut self, size: u64) -> Spec {
            self.size = size;
            self.allocated = size.div_ceil(4096) * 4096;
            self
        }

        pub fn with_flags(mut self, flags: u16) -> Spec {
            self.flags |= flags;
            self
        }

        pub fn modified_at(mut self, filetime: u64) -> Spec {
            self.modified = filetime;
            self
        }
    }

    pub fn file(record: u32, parent: u32, name: &'static str) -> Spec {
        Spec {
            record,
            parent,
            name,
            flags: 0,
            size: 0,
            allocated: 0,
            modified: 0,
        }
    }

    pub fn dir(record: u32, parent: u32, name: &'static str) -> Spec {
        Spec {
            record,
            parent,
            name,
            flags: IS_DIR,
            size: 0,
            allocated: 0,
            modified: 0,
        }
    }

    /// A standalone entry for tests that only exercise the flag/size fields.
    pub fn bare_entry(flags: u16, size: u64, modified: u64) -> Entry {
        Entry {
            record: 20,
            parent: ROOT_RECORD,
            size,
            allocated: size,
            modified,
            name_offset: 0,
            name_len: 0,
            flags,
        }
    }

    pub fn index_from_specs(specs: Vec<Spec>) -> Index {
        let max = specs.iter().map(|s| s.record).max().unwrap_or(0);
        let mut by_record = vec![NO_ENTRY; max as usize + 1];
        let mut names = String::new();
        let mut entries = Vec::with_capacity(specs.len());

        for (i, spec) in specs.iter().enumerate() {
            by_record[spec.record as usize] = i as u32;
            let name_offset = names.len() as u32;
            names.push_str(spec.name);
            entries.push(Entry {
                record: spec.record,
                parent: spec.parent,
                size: spec.size,
                allocated: spec.allocated,
                modified: spec.modified,
                name_offset,
                name_len: spec.name.len() as u16,
                flags: spec.flags,
            });
        }

        Index {
            letter: 'C',
            entries,
            names,
            by_record,
            stats: ScanStats::default(),
        }
    }

    /// Flat index of files, all sitting directly in the root.
    pub fn index_from_names(names: &[&'static str]) -> Index {
        let specs = names
            .iter()
            .enumerate()
            .map(|(i, name)| file(FIRST_USER_RECORD + i as u32, ROOT_RECORD, name))
            .collect();
        index_from_specs(specs)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{dir, file, index_from_names, index_from_specs};
    use super::*;

    #[test]
    fn builds_a_full_path_up_to_the_root() {
        let index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            dir(21, 20, "pc"),
            file(22, 21, "notes.txt"),
        ]);
        assert_eq!(
            index.full_path(2).as_deref(),
            Some(r"C:\Users\pc\notes.txt")
        );
        assert_eq!(index.full_path(0).as_deref(), Some(r"C:\Users"));
        assert_eq!(index.parent_path(2).as_deref(), Some(r"C:\Users\pc"));
    }

    #[test]
    fn a_file_in_the_root_reports_the_root_as_its_parent() {
        let index = index_from_names(&["boot.ini"]);
        assert_eq!(index.parent_path(0).as_deref(), Some(r"C:\"));
    }

    #[test]
    fn a_missing_parent_yields_no_path_instead_of_a_wrong_one() {
        // Parent 99 was never indexed.
        let index = index_from_specs(vec![file(22, 99, "orphan.txt")]);
        assert_eq!(index.full_path(0), None);
    }

    #[test]
    fn a_parent_cycle_terminates() {
        // Two directories claiming each other as parent: must not hang.
        let index = index_from_specs(vec![dir(20, 21, "a"), dir(21, 20, "b")]);
        let _ = index.full_path(0);
    }

    #[test]
    fn names_come_back_from_the_arena() {
        let index = index_from_names(&["alpha.txt", "beta.txt"]);
        assert_eq!(index.name(0), "alpha.txt");
        assert_eq!(index.name(1), "beta.txt");
        assert_eq!(index.name(99), "");
    }

    #[test]
    fn metafiles_are_recognised() {
        let system = |parent: u32, name: &str| ParsedEntry {
            parent,
            name: name.to_string(),
            size: 0,
            allocated: 0,
            modified: 0,
            flags: 0,
        };
        assert!(is_system_record(0, &system(ROOT_RECORD, "$MFT")));
        assert!(is_system_record(11, &system(ROOT_RECORD, "$Extend")));
        // A user file that merely starts with $ deeper in the tree is not one.
        assert!(!is_system_record(500, &system(42, "$recycle-note.txt")));
        assert!(!is_system_record(500, &system(ROOT_RECORD, "Users")));
    }

    #[test]
    fn unreachable_entries_are_dropped_and_the_arena_is_rebuilt() {
        // 20 is fine; 30's parent (99) was never indexed; 31 hangs off 30.
        let mut index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            file(21, 20, "keep.txt"),
            dir(30, 99, "ghost"),
            file(31, 30, "drop.txt"),
        ]);

        let removed = retain_reachable(&mut index.entries, &mut index.names, &mut index.by_record);
        assert_eq!(removed, 2);
        assert_eq!(index.len(), 2);

        // Names must still line up after the arena was compacted.
        assert_eq!(index.name(0), "Users");
        assert_eq!(index.name(1), "keep.txt");
        assert_eq!(index.full_path(1).as_deref(), Some(r"C:\Users\keep.txt"));
        // And the record lookup must point at the new positions.
        assert!(index.by_record(30).is_none());
        assert_eq!(index.by_record(21).map(|(p, _)| p), Some(1));
    }

    #[test]
    fn a_reachable_only_index_is_left_untouched() {
        let mut index =
            index_from_specs(vec![dir(20, ROOT_RECORD, "Users"), file(21, 20, "a.txt")]);
        let removed = retain_reachable(&mut index.entries, &mut index.names, &mut index.by_record);
        assert_eq!(removed, 0);
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn a_cycle_is_treated_as_unreachable() {
        let mut index = index_from_specs(vec![dir(20, 21, "a"), dir(21, 20, "b")]);
        let removed = retain_reachable(&mut index.entries, &mut index.names, &mut index.by_record);
        assert_eq!(removed, 2);
        assert!(index.is_empty());
    }

    fn parsed(parent: u32, name: &str, size: u64) -> ParsedEntry {
        ParsedEntry {
            parent,
            name: name.to_string(),
            size,
            allocated: size,
            modified: 0,
            flags: 0,
        }
    }

    #[test]
    fn upsert_adds_a_new_entry() {
        let mut index = index_from_specs(vec![dir(20, ROOT_RECORD, "Users")]);

        assert!(index.upsert(30, parsed(20, "yeni.txt", 100)));
        assert_eq!(index.len(), 2);
        assert_eq!(index.full_path(1).as_deref(), Some(r"C:\Users\yeni.txt"));
        assert_eq!(index.by_record(30).map(|(p, _)| p), Some(1));
    }

    #[test]
    fn upsert_updates_in_place_and_reports_no_change_when_identical() {
        let mut index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            file(21, 20, "eski.txt"),
        ]);

        // A rename must be visible, and must not disturb the other entry.
        assert!(index.upsert(21, parsed(20, "yeni.txt", 42)));
        assert_eq!(index.name(1), "yeni.txt");
        assert_eq!(index.entries()[1].size, 42);
        assert_eq!(index.name(0), "Users");
        assert_eq!(index.len(), 2);

        // Applying the very same state again is a no-op.
        assert!(!index.upsert(21, parsed(20, "yeni.txt", 42)));
    }

    #[test]
    fn deleting_hides_an_entry_without_moving_the_others() {
        let mut index = index_from_specs(vec![
            dir(20, ROOT_RECORD, "Users"),
            file(21, 20, "a.txt"),
            file(22, 20, "b.txt"),
        ]);

        assert!(index.mark_deleted(21));
        assert!(index.entries()[1].is_deleted());
        // Positions must survive, or the prebuilt search arena would be wrong.
        assert_eq!(index.name(2), "b.txt");
        assert!(!index.entries()[2].is_deleted());

        // Deleting twice changes nothing.
        assert!(!index.mark_deleted(21));
        // Deleting something that was never indexed is harmless.
        assert!(!index.mark_deleted(999));
    }

    #[test]
    fn the_counts_track_live_changes() {
        let mut index =
            index_from_specs(vec![dir(20, ROOT_RECORD, "Users"), file(21, 20, "a.txt")]);
        index.stats.dirs = 1;
        index.stats.files = 1;

        index.upsert(22, parsed(20, "b.txt", 0));
        assert_eq!((index.stats.files, index.stats.dirs), (2, 1));

        index.mark_deleted(21);
        assert_eq!((index.stats.files, index.stats.dirs), (1, 1));

        // Deleting twice must not double-count.
        index.mark_deleted(21);
        assert_eq!(index.stats.files, 1);

        // And bringing it back counts once.
        index.upsert(21, parsed(20, "a.txt", 0));
        assert_eq!(index.stats.files, 2);
    }

    #[test]
    fn a_recycled_record_comes_back_to_life() {
        let mut index =
            index_from_specs(vec![dir(20, ROOT_RECORD, "Users"), file(21, 20, "a.txt")]);
        index.mark_deleted(21);

        // NTFS reuses record numbers; the slot must be usable again.
        assert!(index.upsert(21, parsed(20, "baska.txt", 7)));
        assert!(!index.entries()[1].is_deleted());
        assert_eq!(index.name(1), "baska.txt");
    }

    #[test]
    fn progress_is_a_clamped_fraction() {
        let at = |done, total| {
            Progress {
                records_done: done,
                records_total: total,
            }
            .fraction()
        };

        assert_eq!(at(0, 100), 0.0);
        assert_eq!(at(50, 100), 0.5);
        assert_eq!(at(100, 100), 1.0);
        // The table can grow mid-scan, so the count can pass the estimate.
        assert_eq!(at(130, 100), 1.0);
        // And an unknown total must not divide by zero.
        assert_eq!(at(10, 0), 0.0);
    }

    #[test]
    fn converts_filetime_to_unix_seconds() {
        // 1970-01-01 as a FILETIME.
        assert_eq!(filetime_to_unix(116_444_736_000_000_000), Some(0));
        assert_eq!(filetime_to_unix(0), None);
        // 2001-01-01T00:00:00Z
        assert_eq!(filetime_to_unix(126_227_808_000_000_000), Some(978_307_200));
    }

    #[test]
    fn entry_stays_small() {
        // The whole memory story depends on this; 2 M entries * 40 B = 80 MB.
        // The extra 8 bytes over Ferret's search index are the on-disk size,
        // which a disk-usage view cannot do without.
        assert_eq!(std::mem::size_of::<Entry>(), 40);
    }

    fn name(parent: u64, text: &str, namespace: u8) -> record::FileName {
        let mut value = vec![0u8; 0x42];
        value[0..8].copy_from_slice(&parent.to_le_bytes());
        value[0x28..0x30].copy_from_slice(&4096u64.to_le_bytes());
        value[0x30..0x38].copy_from_slice(&7u64.to_le_bytes());
        value[0x40] = text.encode_utf16().count() as u8;
        value[0x41] = namespace;
        for ch in text.encode_utf16() {
            value.extend_from_slice(&ch.to_le_bytes());
        }
        record::parse_file_name(&value).unwrap()
    }

    #[test]
    fn a_stitched_file_takes_its_size_from_the_extension_record() {
        // Base record: names and standard info, but $DATA moved elsewhere.
        let mut base = Parts {
            name: Some(name(ROOT_RECORD as u64, "disk.vhdx", 1)),
            links: 1,
            ..Parts::default()
        };
        let extension = Parts {
            data: Some((40 << 30, 41 << 30)),
            ..Parts::default()
        };
        base.absorb(extension);

        let entry = base.finish().unwrap();
        assert_eq!(entry.size, 40 << 30);
        assert_eq!(entry.allocated, 41 << 30);
        assert_eq!(entry.flags & IS_HARDLINK, 0);
    }

    #[test]
    fn without_any_data_stream_the_directory_entry_sizes_are_the_fallback() {
        let parts = Parts {
            name: Some(name(ROOT_RECORD as u64, "a.txt", 3)),
            links: 1,
            ..Parts::default()
        };
        let entry = parts.finish().unwrap();
        assert_eq!((entry.size, entry.allocated), (7, 4096));
    }

    #[test]
    fn two_real_names_mark_a_hard_link_but_an_alias_does_not() {
        let mut linked = Parts {
            name: Some(name(ROOT_RECORD as u64, "one.dll", 1)),
            links: 1,
            ..Parts::default()
        };
        linked.absorb(Parts {
            name: Some(name(40, "two.dll", 0)),
            links: 1,
            ..Parts::default()
        });
        let entry = linked.finish().unwrap();
        assert!(entry.flags & IS_HARDLINK != 0);
        // The first name found keeps the file; it is counted once.
        assert_eq!(entry.name, "one.dll");

        let mut aliased = Parts {
            name: Some(name(ROOT_RECORD as u64, "PROGRA~1", 2)),
            links: 0,
            ..Parts::default()
        };
        aliased.absorb(Parts {
            name: Some(name(ROOT_RECORD as u64, "Program Files", 1)),
            links: 1,
            ..Parts::default()
        });
        let entry = aliased.finish().unwrap();
        assert_eq!(entry.flags & IS_HARDLINK, 0);
        assert_eq!(entry.name, "Program Files");
    }

    #[test]
    fn streams_and_indexes_add_to_the_on_disk_size() {
        let parts = Parts {
            name: Some(name(ROOT_RECORD as u64, "a.txt", 1)),
            links: 1,
            data: Some((100, 4096)),
            other_on_disk: 8192,
            ..Parts::default()
        };
        let entry = parts.finish().unwrap();
        assert_eq!((entry.size, entry.allocated), (100, 12288));
    }

    #[test]
    fn a_record_with_no_name_is_not_an_entry() {
        assert!(Parts::default().finish().is_none());
    }
}
