//! `ferret-disk-cli report C` - where the space on a drive went.
//! `ferret-disk-cli verify C` - check the engine's sizes against Windows.
//!
//! Both need administrator rights, because both read the raw volume.

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::process::ExitCode;
use std::time::Instant;

use ferret_core::mft::{IS_COMPRESSED, IS_SPARSE};
use ferret_core::{human_size, Index, ScanOptions};
use ferret_tree::{NodeId, Tree};
use windows_sys::Win32::Storage::FileSystem::{
    FileStandardInfo, GetDiskFreeSpaceExW, GetFileInformationByHandleEx,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_STANDARD_INFO,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let letter = args
        .get(1)
        .and_then(|a| a.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('C');
    let number = |flag: &str, default: usize| -> usize {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };

    let result = match command {
        "report" => report(letter, number("--top", 20)),
        "verify" => verify(letter, number("--sample", 3000)),
        _ => {
            eprintln!("usage: ferret-disk-cli report <drive> [--top N]");
            eprintln!("       ferret-disk-cli verify <drive> [--sample N]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Scanned {
    index: Index,
    tree: Tree,
}

fn scan(letter: char) -> Result<Scanned, String> {
    let started = Instant::now();
    let options = ScanOptions {
        include_system: true,
    };
    let index = ferret_core::scan_with(letter, options).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "reading the raw volume needs administrator rights".to_string()
        } else {
            e.to_string()
        }
    })?;
    let scanned = started.elapsed();

    let started = Instant::now();
    let tree = Tree::build(&index);
    let built = started.elapsed();

    let s = &index.stats;
    println!(
        "{letter}: {} files, {} folders - MFT read in {:.2}s, parsed in {:.2}s total, tree in {} ms",
        s.files,
        s.dirs,
        s.read_time.as_secs_f64(),
        scanned.as_secs_f64(),
        built.as_millis()
    );
    println!(
        "   records: {} in use, {} stitched from extension records, {} damaged, {} orphaned",
        s.records_in_use, s.records_stitched, s.records_damaged, s.records_orphaned
    );
    println!(
        "   memory: index {}, tree {}",
        human_size(index.memory_bytes() as u64),
        human_size(tree.memory_bytes() as u64)
    );
    Ok(Scanned { index, tree })
}

fn report(letter: char, top: usize) -> Result<ExitCode, String> {
    let Scanned { index, tree } = scan(letter)?;
    let root = tree.totals(tree.root());

    if let Some((total, free)) = disk_space(letter) {
        let used = total - free;
        println!();
        println!(
            "Volume: {} total, {} used, {} free",
            human_size(total),
            human_size(used),
            human_size(free)
        );
        println!(
            "Found:  {} on disk ({} logical) - {:.1}% of used space accounted for",
            human_size(root.allocated),
            human_size(root.size),
            root.allocated as f64 * 100.0 / used.max(1) as f64
        );
        if used > root.allocated {
            println!(
                "        {} not in any file: shadow copies, restore points, free-space metadata",
                human_size(used - root.allocated)
            );
        }
    }

    println!("\nTop level:");
    for &child in tree.children(tree.root()).iter().take(top) {
        print_row(
            &index,
            &tree,
            child,
            &tree.name(&index, child),
            root.allocated,
        );
    }

    println!("\nLargest files:");
    for node in tree.largest_files(top) {
        print_row(
            &index,
            &tree,
            node,
            &tree.path(&index, node),
            root.allocated,
        );
    }

    println!("\nFolders holding the most directly:");
    for (node, bytes) in tree.heaviest_folders(top) {
        println!("  {:>10}  {}", human_size(bytes), tree.path(&index, node));
    }

    println!("\nBy kind:");
    for (kind, totals) in tree.kinds_under(&index, tree.root()) {
        println!(
            "  {:>10}  {:>9} files  {}",
            human_size(totals.allocated),
            totals.files,
            kind.key()
        );
    }

    println!("\nBy extension:");
    for (ext, totals) in tree.extensions_under(&index, tree.root()).iter().take(top) {
        let label = if ext.is_empty() {
            "(none)"
        } else {
            ext.as_str()
        };
        println!(
            "  {:>10}  {:>9} files  .{label}",
            human_size(totals.allocated),
            totals.files
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn print_row(_index: &Index, tree: &Tree, node: NodeId, label: &str, whole: u64) {
    let t = tree.totals(node);
    let share = t.allocated as f64 * 100.0 / whole.max(1) as f64;
    if tree.is_dir(node) {
        println!(
            "  {:>10}  {:>5.1}%  {:>9} files  {label}\\",
            human_size(t.allocated),
            share,
            t.files
        );
    } else {
        println!(
            "  {:>10}  {:>5.1}%  {label}",
            human_size(t.allocated),
            share
        );
    }
}

/// How one file's sizes compare with what Windows reports.
#[derive(Default)]
struct Tally {
    checked: usize,
    unopenable: usize,
    size_equal: usize,
    alloc_equal: usize,
    /// Small files that live inside their MFT record. They occupy no
    /// clusters, which is what the engine says; Windows reports their size
    /// rounded up to 8 bytes instead.
    alloc_resident: usize,
    /// Compressed or sparse files, where the engine counts stored clusters
    /// and Windows' standard information counts the full allocation.
    alloc_packed: usize,
    /// The engine counted more: alternate data streams, which Windows leaves
    /// out of a file's standard information.
    alloc_more: usize,
    alloc_less: usize,
    links_agree: usize,
}

fn verify(letter: char, sample: usize) -> Result<ExitCode, String> {
    let Scanned { index, tree } = scan(letter)?;

    // The largest files matter most - they are the fragmented ones that
    // need stitching - and a spread of random ones catches everything else.
    let mut picks: Vec<NodeId> = tree.largest_files(sample / 4);
    let files: Vec<NodeId> = (0..tree.root())
        .filter(|n| tree.contains(*n) && !tree.is_dir(*n))
        .collect();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..(sample - picks.len()).min(files.len()) {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        picks.push(files[(seed % files.len() as u64) as usize]);
    }
    picks.sort_unstable();
    picks.dedup();

    let mut tally = Tally::default();
    let mut worst: Vec<(i64, String, u64, u64, u64, u64)> = Vec::new();
    for node in picks {
        let entry = index.entries()[node as usize];
        let path = tree.path(&index, node);
        let Some(info) = standard_info(&path) else {
            tally.unopenable += 1;
            continue;
        };
        tally.checked += 1;
        let theirs_size = info.EndOfFile as u64;
        let theirs_alloc = info.AllocationSize as u64;

        if entry.size == theirs_size {
            tally.size_equal += 1;
        }
        if (info.NumberOfLinks > 1) == entry.is_hardlink() {
            tally.links_agree += 1;
        }
        let packed = entry.flags & (IS_COMPRESSED | IS_SPARSE) != 0;
        let unexplained = match entry.allocated.cmp(&theirs_alloc) {
            std::cmp::Ordering::Equal => {
                tally.alloc_equal += 1;
                false
            }
            std::cmp::Ordering::Greater => {
                tally.alloc_more += 1;
                false
            }
            std::cmp::Ordering::Less if entry.allocated == 0 && theirs_alloc < 1024 => {
                tally.alloc_resident += 1;
                false
            }
            std::cmp::Ordering::Less if packed => {
                tally.alloc_packed += 1;
                false
            }
            std::cmp::Ordering::Less => {
                tally.alloc_less += 1;
                true
            }
        };
        let off = (entry.size as i64 - theirs_size as i64)
            .abs()
            .max((entry.allocated as i64 - theirs_alloc as i64).abs());
        if off > 0 && (unexplained || entry.size != theirs_size) {
            worst.push((
                off,
                path,
                entry.size,
                theirs_size,
                entry.allocated,
                theirs_alloc,
            ));
        }
    }

    let pct = |n: usize| n as f64 * 100.0 / tally.checked.max(1) as f64;
    println!();
    println!(
        "Checked {} files ({} could not be opened - in use or protected)",
        tally.checked, tally.unopenable
    );
    println!("  size identical:        {:>6.2}%", pct(tally.size_equal));
    println!("  on-disk identical:     {:>6.2}%", pct(tally.alloc_equal));
    println!(
        "  inside the MFT:        {:>6.2}%  (0 clusters; Windows rounds the size up instead)",
        pct(tally.alloc_resident)
    );
    println!(
        "  compressed/sparse:     {:>6.2}%  (stored clusters, below Windows' full allocation)",
        pct(tally.alloc_packed)
    );
    println!(
        "  on-disk higher:        {:>6.2}%  (alternate data streams, not in Windows' figure)",
        pct(tally.alloc_more)
    );
    println!(
        "  on-disk lower:         {:>6.2}%  (unexplained)",
        pct(tally.alloc_less)
    );
    println!("  hard-link flag agrees: {:>6.2}%", pct(tally.links_agree));

    worst.sort_unstable_by_key(|w| std::cmp::Reverse(w.0));
    if !worst.is_empty() {
        println!("\nLargest disagreements (size ours/windows, on disk ours/windows):");
        for (_, path, s1, s2, a1, a2) in worst.iter().take(15) {
            println!(
                "  {} / {}   {} / {}   {path}",
                human_size(*s1),
                human_size(*s2),
                human_size(*a1),
                human_size(*a2)
            );
        }
    }

    // Files change between the scan and the check - logs grow, caches churn —
    // so a perfect score is not expected. 98 % is.
    let explained =
        tally.alloc_equal + tally.alloc_resident + tally.alloc_packed + tally.alloc_more;
    let passed = pct(tally.size_equal) >= 98.0 && pct(explained) >= 98.0;
    println!("\n{}", if passed { "PASS" } else { "FAIL" });
    Ok(if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// What Windows says about a file's sizes and links.
fn standard_info(path: &str) -> Option<FILE_STANDARD_INFO> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as _,
            FileStandardInfo,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    (ok != 0).then_some(info)
}

/// `(total, free)` bytes of a volume.
fn disk_space(letter: char) -> Option<(u64, u64)> {
    let root: Vec<u16> = std::ffi::OsStr::new(&format!("{letter}:\\"))
        .encode_wide()
        .chain(Some(0))
        .collect();
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0).then_some((total, free))
}
