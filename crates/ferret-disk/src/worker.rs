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
use ferret_tree::Tree;

use crate::shell::{self, Drive};

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
}

pub enum Event {
    Drives(Vec<Drive>),
    Progress {
        letter: char,
        percent: u8,
    },
    Scanned(char, Result<SharedScan, String>),
    Elevation(bool),
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
                sink.send(Event::Scanned(letter, result));
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

fn scan(letter: char, sink: &Sink) -> Result<SharedScan, String> {
    let started = Instant::now();
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

    Ok(Arc::new(RwLock::new(Scan {
        letter,
        index,
        tree,
        drive,
        seconds: started.elapsed().as_secs_f64(),
        generation: 1,
    })))
}
