//! The thread that does the slow work.
//!
//! egui redraws on one thread and expects each frame to take a millisecond or
//! two; a scan takes seconds. So the window sends a [`Request`] and draws
//! whatever [`Event`]s have come back. Every event is followed by a repaint
//! request, because egui otherwise sleeps until the mouse moves.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use eframe::egui;
use ferret_core::{Index, ScanOptions};
use ferret_tree::{cleanup, dupes, snapshot, Tree};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::shell::{self, Drive};
use crate::snapshots;
use crate::watch::{self, WatchEvent};
use ferret_core::journal::{self, Cursor};

/// One scanned volume. Shared with the window behind a lock, so the worker
/// can later update it in place as the disk changes.
pub struct Scan {
    pub letter: char,
    pub index: Index,
    pub tree: Tree,
    /// The volume's size and free space when the scan finished.
    pub drive: Drive,
    pub seconds: f64,
    /// Bumped whenever the tree changes, so cached layouts know to redo.
    pub generation: u64,
    /// Whether the change journal is being followed.
    pub live: bool,
    /// Changes folded in since the scan.
    pub changes: u64,
}

impl Scan {
    pub fn memory_bytes(&self) -> usize {
        self.index.memory_bytes() + self.tree.memory_bytes()
    }
}

pub type SharedScan = Arc<RwLock<Scan>>;

pub enum Request {
    Drives,
    Scan(char),
    Open(String),
    Reveal(String),
    CheckElevation,
    RestartElevated,
    /// Look for duplicate files in a scan, on a thread of its own so the
    /// worker stays free. Stops early when cancel is set.
    FindDuplicates {
        scan: SharedScan,
        min_size: u64,
        cancel: Arc<AtomicBool>,
    },
    /// Run the cleanup rules over a scan.
    Suggest(SharedScan),
    /// Move paths to the recycle bin; ytes is what they hold.
    Recycle {
        paths: Vec<String>,
        bytes: u64,
    },
    OpenRecycleBin,
    OpenDiskCleanup,
    /// The saved snapshots of a drive.
    ListSnapshots(char),
    /// Compare a saved snapshot with the scan as it is now.
    Compare {
        scan: SharedScan,
        saved: snapshots::Saved,
    },
}

pub enum Event {
    Drives(Vec<Drive>),
    Progress {
        letter: char,
        percent: u8,
    },
    Scanned(char, Result<SharedScan, String>),
    Elevation(bool),
    /// The disk changed and the scan was updated in place.
    Changed,
    /// The change journal can no longer be followed; a rescan is needed.
    Stale,
    DupesProgress(dupes::Progress),
    Snapshots(Vec<snapshots::Saved>),
    Compared {
        saved: snapshots::Saved,
        /// Used space now, to set against the snapshot's.
        used_now: u64,
        changes: Vec<snapshot::Change>,
    },
    Suggestions {
        generation: u64,
        list: Vec<cleanup::Suggestion>,
    },
    /// What a recycle achieved. ytes is what was asked to go: the space
    /// only comes back when the recycle bin is emptied.
    Recycled {
        result: shell::Recycled,
        bytes: u64,
    },
    DupesFound {
        groups: Vec<dupes::Group>,
        seconds: f64,
        cancelled: bool,
    },
    Failed(String),
    /// An elevated copy is starting; this one should close.
    Restarting,
}

pub struct Worker {
    requests: Sender<Request>,
    events: Receiver<Event>,
}

impl Worker {
    pub fn start(ctx: egui::Context) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        std::thread::Builder::new()
            .name("ferret-disk-worker".into())
            .spawn(move || {
                run(
                    request_rx,
                    Sink {
                        events: event_tx,
                        ctx,
                    },
                )
            })
            .expect("could not start the worker thread");
        Self {
            requests: request_tx,
            events: event_rx,
        }
    }

    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    pub fn drain(&self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }
}

#[derive(Clone)]
struct Sink {
    events: Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

fn run(requests: Receiver<Request>, sink: Sink) {
    while let Ok(request) = requests.recv() {
        match request {
            Request::Drives => sink.send(Event::Drives(shell::drives())),
            Request::Scan(letter) => {
                let result = scan(letter, &sink);
                if let Ok((shared, _)) = &result {
                    // Record this scan for later comparison, off the worker so
                    // the window gets its map first.
                    let shared = shared.clone();
                    let sink = sink.clone();
                    let _ = std::thread::Builder::new()
                        .name("ferret-disk-snapshot".into())
                        .spawn(move || {
                            let snap = shared.read().ok().map(|s| {
                                snapshot::capture(&s.index, &s.tree, s.drive.used(), unix_now())
                            });
                            if let Some(snap) = snap {
                                let _ = snapshots::save(&snap);
                                sink.send(Event::Snapshots(snapshots::list(letter)));
                            }
                        });
                }
                if let Ok((shared, Some(cursor))) = &result {
                    let watch_sink = sink.clone();
                    watch::spawn(Arc::downgrade(shared), letter, *cursor, move |event| {
                        watch_sink.send(match event {
                            WatchEvent::Updated => Event::Changed,
                            WatchEvent::Stale => Event::Stale,
                        })
                    });
                }
                sink.send(Event::Scanned(letter, result.map(|(shared, _)| shared)));
            }
            Request::Open(path) => {
                if let Err(err) = shell::open(&path) {
                    sink.send(Event::Failed(err));
                }
            }
            Request::Reveal(path) => {
                if let Err(err) = shell::reveal(&path) {
                    sink.send(Event::Failed(err));
                }
            }
            Request::FindDuplicates {
                scan,
                min_size,
                cancel,
            } => {
                let sink = sink.clone();
                let _ = std::thread::Builder::new()
                    .name("ferret-disk-dupes".into())
                    .spawn(move || find_duplicates(&scan, min_size, &cancel, &sink));
            }
            Request::Suggest(scan) => {
                let found = scan.read().ok().map(|s| {
                    let now = now_filetime();
                    (s.generation, cleanup::suggest(&s.index, &s.tree, now))
                });
                if let Some((generation, list)) = found {
                    sink.send(Event::Suggestions { generation, list });
                }
            }
            Request::Recycle { paths, bytes } => {
                let result = shell::recycle(&paths);
                sink.send(Event::Recycled { result, bytes });
            }
            Request::ListSnapshots(letter) => sink.send(Event::Snapshots(snapshots::list(letter))),
            Request::Compare { scan, saved } => {
                let now = scan.read().ok().map(|s| {
                    let used = s.drive.used();
                    (snapshot::capture(&s.index, &s.tree, used, unix_now()), used)
                });
                if let (Some((now, used_now)), Some(then)) = (now, snapshots::load(&saved)) {
                    let changes = snapshot::diff(&then, &now, 10 << 20);
                    sink.send(Event::Compared {
                        saved,
                        used_now,
                        changes,
                    });
                }
            }
            Request::OpenRecycleBin => {
                if let Err(err) = shell::open_recycle_bin() {
                    sink.send(Event::Failed(err));
                }
            }
            Request::OpenDiskCleanup => {
                if let Err(err) = shell::open_disk_cleanup() {
                    sink.send(Event::Failed(err));
                }
            }
            Request::CheckElevation => sink.send(Event::Elevation(shell::is_elevated())),
            Request::RestartElevated => match shell::restart_elevated() {
                Ok(()) => {
                    sink.send(Event::Restarting);
                    return;
                }
                Err(err) => sink.send(Event::Failed(err)),
            },
        }
    }
}

/// Progress is passed on no more often than this; the engine reports far
/// more often than a bar can show.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

fn scan(letter: char, sink: &Sink) -> Result<(SharedScan, Option<Cursor>), String> {
    let started = Instant::now();
    // Where the change journal stands *before* the table is read, so nothing
    // that happens during the scan is missed. No journal just means no live
    // updates.
    let cursor = ferret_core::Volume::open(letter)
        .ok()
        .and_then(|volume| journal::cursor_at_end(&volume).ok());
    let mut last = Instant::now() - PROGRESS_EVERY;
    // Metafiles included: `$MFT` alone is gigabytes on a big disk, and a
    // disk-usage view that hid it would leave that space unexplained.
    let options = ScanOptions {
        include_system: true,
    };
    let index = ferret_core::scan_with_progress(letter, options, |progress| {
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            sink.send(Event::Progress {
                letter,
                percent: (progress.fraction() * 100.0).round() as u8,
            });
        }
    })
    .map_err(|err| {
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            "needs_elevation".to_string()
        } else {
            format!("scan_failed:{err}")
        }
    })?;

    let tree = Tree::build(&index);
    let drive = shell::drive(letter).unwrap_or(Drive {
        letter,
        label: String::new(),
        filesystem: "NTFS".into(),
        total: 0,
        free: 0,
    });

    let shared = Arc::new(RwLock::new(Scan {
        letter,
        index,
        tree,
        drive,
        seconds: started.elapsed().as_secs_f64(),
        generation: 1,
        live: cursor.is_some(),
        changes: 0,
    }));
    Ok((shared, cursor))
}

fn find_duplicates(scan: &SharedScan, min_size: u64, cancel: &AtomicBool, sink: &Sink) {
    let started = Instant::now();
    // The list of candidates needs the index; the reading does not, so the
    // lock is held only for the first.
    let candidates = match scan.read() {
        Ok(scan) => dupes::candidates(&scan.index, &scan.tree, min_size),
        Err(_) => return,
    };
    let last = std::sync::Mutex::new(Instant::now() - PROGRESS_EVERY);
    let groups = dupes::find(candidates, &dupes::Disk, cancel, &|progress| {
        if let Ok(mut last) = last.lock() {
            if last.elapsed() >= PROGRESS_EVERY {
                *last = Instant::now();
                sink.send(Event::DupesProgress(progress));
            }
        }
    });
    sink.send(Event::DupesFound {
        groups,
        seconds: started.elapsed().as_secs_f64(),
        cancelled: cancel.load(Ordering::Relaxed),
    });
}

/// The current time as a FILETIME, the unit the index keeps times in.
fn now_filetime() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() + 11_644_473_600) * 10_000_000)
        .unwrap_or(0)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
