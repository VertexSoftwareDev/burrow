//! The window: a folder tree on the left, the treemap on the right, the
//! drive's usage across the top.
//!
//! The two views are one selection. Clicking a tile selects its row and opens
//! the folders above it; clicking a row outlines its tile. The map can be
//! focused on any folder, and the tree keeps showing the whole disk.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use burrow_tree::{cleanup, dupes, snapshot, Kind, NodeId, Totals};
use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::format;
use crate::i18n::Lang;
use crate::prefs::{self, Metric, Prefs};
use crate::rows::{self, Row};
use crate::shell::{self, Drive};
use crate::snapshots;
use crate::theme::{self, Theme, ROW_HEIGHT};
use crate::treemap::{self, Layout, What};
use crate::worker::{Event, Request, Scan, SharedScan, Worker};

enum Phase {
    Starting,
    NeedsElevation,
    Scanning { letter: char, percent: u8 },
    Ready,
    Failed { letter: char, message: String },
    NoVolume,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Folders,
    Largest,
    Kinds,
    Duplicates,
    Cleanup,
    Changes,
}

/// The changes tab: saved snapshots, which one is compared, and the result.
struct ChangesView {
    saved: Vec<snapshots::Saved>,
    chosen: Option<std::path::PathBuf>,
    result: Option<(snapshots::Saved, u64, Vec<snapshot::Change>)>,
    /// (generation, snapshot) the result was computed for.
    computed_for: Option<(u64, std::path::PathBuf)>,
    computing: bool,
    asked: Option<Instant>,
}

impl ChangesView {
    fn new() -> Self {
        Self {
            saved: Vec::new(),
            chosen: None,
            result: None,
            computed_for: None,
            computing: false,
            asked: None,
        }
    }
}

/// The cleanup tab: the rules' latest findings and which of them are ticked.
struct CleanupView {
    list: Vec<cleanup::Suggestion>,
    /// Scan generation the list was computed for.
    generation: Option<u64>,
    computing: bool,
    asked: Option<Instant>,
    /// Rule ids ticked for removal.
    chosen: HashSet<&'static str>,
    /// Rule ids whose items are listed.
    open: HashSet<&'static str>,
    /// Whether the safe rules have been ticked by default yet.
    primed: bool,
}

impl CleanupView {
    fn new() -> Self {
        Self {
            list: Vec::new(),
            generation: None,
            computing: false,
            asked: None,
            chosen: HashSet::new(),
            open: HashSet::new(),
            primed: false,
        }
    }
}

/// A removal waiting for the person to say yes.
struct Confirm {
    paths: Vec<String>,
    bytes: u64,
}

/// The duplicates tab: a search that can be running, and what it found.
struct DupesView {
    min_size: u64,
    /// Set while a search runs; setting the flag stops it.
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<dupes::Progress>,
    groups: Vec<dupes::Group>,
    /// (seconds, cancelled) of the last finished search.
    finished: Option<(f64, bool)>,
    collapsed: HashSet<usize>,
    rows: Vec<DupRow>,
    /// Paths ticked for removal.
    chosen: HashSet<String>,
}

#[derive(Clone, Copy)]
enum DupRow {
    Group(usize),
    File(usize, usize),
}

impl DupesView {
    fn new() -> Self {
        Self {
            min_size: 1 << 20,
            cancel: None,
            progress: None,
            groups: Vec::new(),
            finished: None,
            collapsed: HashSet::new(),
            rows: Vec::new(),
            chosen: HashSet::new(),
        }
    }

    fn stop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    fn relayout(&mut self) {
        self.rows.clear();
        for (g, group) in self.groups.iter().enumerate() {
            self.rows.push(DupRow::Group(g));
            if !self.collapsed.contains(&g) {
                self.rows
                    .extend((0..group.files.len()).map(|f| DupRow::File(g, f)));
            }
        }
    }
}

/// Something a view wants done. Collected while drawing — the views borrow
/// the scan they draw — and carried out afterwards.
enum Action {
    /// Select a node; `from_map` also opens its folders in the tree and
    /// scrolls to it.
    Select {
        node: NodeId,
        from_map: bool,
    },
    Toggle(NodeId),
    Focus(NodeId),
    Open(String),
    Reveal(String),
    Copy(String),
    /// Ask to move these to the recycle bin; ytes is what they hold.
    Recycle {
        paths: Vec<String>,
        bytes: u64,
    },
}

pub struct DiskApp {
    worker: Worker,
    lang: Lang,
    theme: Theme,
    metric: Metric,
    phase: Phase,
    drives: Vec<Drive>,
    letter: char,
    /// The drive the user last chose, from the previous run.
    remembered: String,
    scan: Option<SharedScan>,
    tab: Tab,

    expanded: HashSet<NodeId>,
    rows: Vec<Row>,
    /// `(generation, metric)` the rows were flattened for.
    rows_for: Option<(u64, Metric)>,
    selected: Option<NodeId>,
    scroll_to_row: bool,

    focus: NodeId,
    /// The tree's root id, which moves when live changes add entries.
    root: NodeId,
    layout: Option<Layout>,
    hovered: Option<NodeId>,
    /// What the map's context menu is about.
    menu_node: Option<NodeId>,

    largest: Option<(u64, Metric, Vec<NodeId>)>,
    kinds: Option<(u64, NodeId, Vec<(Kind, Totals)>, Vec<(String, Totals)>)>,

    dupes: DupesView,
    cleanup: CleanupView,
    changes: ChangesView,
    /// When the current scan finished, in Unix seconds.
    scanned_at: u64,
    confirm: Option<Confirm>,
    recycling: bool,

    toast: Option<(String, Instant)>,
    title: String,
    shot: Option<Shot>,
}

/// What `--screenshot` asked for.
pub struct Screenshot {
    pub path: std::path::PathBuf,
    pub tab: Option<String>,
    /// `tr` or `en`, overriding the saved language for this run.
    pub lang: Option<String>,
}

/// A pending `--screenshot`: wait for the scan and a few settled frames,
/// ask for a capture, save it, close.
struct Shot {
    path: std::path::PathBuf,
    /// Which tab to show; the duplicates tab also runs its search first.
    tab: Option<String>,
    frames_ready: u32,
    requested: bool,
}

impl DiskApp {
    pub fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<Screenshot>) -> Self {
        let prefs: Prefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, prefs::KEY))
            .unwrap_or_default();

        theme::install_fonts(&cc.egui_ctx);
        theme::apply_style(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, prefs.theme);

        let worker = Worker::start(cc.egui_ctx.clone());
        worker.send(Request::Drives);
        worker.send(Request::CheckElevation);

        Self {
            worker,
            lang: match screenshot.as_ref().and_then(|s| s.lang.as_deref()) {
                Some("en") => Lang::En,
                Some("tr") => Lang::Tr,
                _ => prefs.lang,
            },
            theme: prefs.theme,
            metric: prefs.metric,
            phase: Phase::Starting,
            drives: Vec::new(),
            letter: shell::system_drive(),
            remembered: prefs.drive,
            scan: None,
            tab: Tab::Folders,
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_for: None,
            selected: None,
            scroll_to_row: false,
            focus: 0,
            root: 0,
            layout: None,
            hovered: None,
            menu_node: None,
            largest: None,
            kinds: None,
            dupes: DupesView::new(),
            cleanup: CleanupView::new(),
            changes: ChangesView::new(),
            scanned_at: 0,
            confirm: None,
            recycling: false,
            toast: None,
            title: String::new(),
            shot: screenshot.map(|s| Shot {
                path: s.path,
                tab: s.tab,
                frames_ready: 0,
                requested: false,
            }),
        }
    }

    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        let captured = ctx.input(|input| {
            input.raw.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = captured {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            let _ = image::save_buffer(
                &shot.path,
                &bytes,
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            );
            self.shot = None;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let mut settled = matches!(
            self.phase,
            Phase::Ready | Phase::NeedsElevation | Phase::Failed { .. } | Phase::NoVolume
        );
        if settled && matches!(self.phase, Phase::Ready) {
            match shot.tab.as_deref() {
                Some("largest") => self.tab = Tab::Largest,
                Some("kinds") => self.tab = Tab::Kinds,
                Some("changes") => {
                    self.tab = Tab::Changes;
                    settled = self.changes.result.is_some() && !self.changes.computing;
                }
                Some("cleanup") => {
                    self.tab = Tab::Cleanup;
                    settled = self.cleanup.generation.is_some() && !self.cleanup.computing;
                }
                Some("duplicates") => {
                    self.tab = Tab::Duplicates;
                    if self.dupes.cancel.is_none() && self.dupes.finished.is_none() {
                        let cancel = Arc::new(AtomicBool::new(false));
                        self.dupes.cancel = Some(cancel.clone());
                        self.dupes.min_size = 10 << 20;
                        if let Some(scan) = self.scan.clone() {
                            self.worker.send(Request::FindDuplicates {
                                scan,
                                min_size: 10 << 20,
                                cancel,
                            });
                        }
                    }
                    settled = self.dupes.finished.is_some();
                }
                _ => {}
            }
        }
        if settled && !shot.requested {
            shot.frames_ready += 1;
            if shot.frames_ready >= 5 {
                shot.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
        }
        ctx.request_repaint();
    }

    fn prefs(&self) -> Prefs {
        Prefs {
            lang: self.lang,
            theme: self.theme,
            drive: self.letter.to_string(),
            metric: self.metric,
        }
    }

    fn start_scan(&mut self) {
        self.phase = Phase::Scanning {
            letter: self.letter,
            percent: 0,
        };
        self.worker.send(Request::Scan(self.letter));
    }

    fn inform(&mut self, message: String) {
        self.toast = Some((message, Instant::now()));
    }

    /// The watcher rebuilt the tree. Everything keyed by generation redraws
    /// by itself; what needs care is the root, whose id is one past the last
    /// entry and so moves when new files are indexed, and a selection whose
    /// file has since been deleted.
    fn after_change(&mut self) {
        let Some(shared) = self.scan.clone() else {
            return;
        };
        let Ok(scan) = shared.read() else { return };
        let tree = &scan.tree;
        let root = tree.root();
        if root != self.root {
            let old = self.root;
            if self.expanded.remove(&old) {
                self.expanded.insert(root);
            }
            if self.focus == old {
                self.focus = root;
            }
            if self.selected == Some(old) {
                self.selected = Some(root);
            }
            self.root = root;
        }
        if self.selected.is_some_and(|s| !tree.contains(s)) {
            self.selected = None;
        }
        if !tree.contains(self.focus) || !tree.is_dir(self.focus) {
            self.focus = root;
        }
        self.menu_node = None;
    }

    fn absorb(&mut self, events: Vec<Event>) {
        for event in events {
            match event {
                Event::Drives(drives) => {
                    let wanted = self.remembered.chars().next();
                    let ntfs =
                        |letter: char| drives.iter().any(|d| d.letter == letter && d.is_ntfs());
                    if let Some(letter) = wanted.filter(|l| ntfs(*l)) {
                        self.letter = letter;
                    } else if !ntfs(self.letter) {
                        match drives.iter().find(|d| d.is_ntfs()) {
                            Some(d) => self.letter = d.letter,
                            None => self.phase = Phase::NoVolume,
                        }
                    }
                    self.drives = drives;
                }
                Event::Elevation(true) => {
                    if matches!(self.phase, Phase::Starting) {
                        self.start_scan();
                    }
                }
                Event::Elevation(false) => self.phase = Phase::NeedsElevation,
                Event::Progress { letter, percent } => {
                    if let Phase::Scanning {
                        letter: l,
                        percent: p,
                    } = &mut self.phase
                    {
                        if *l == letter {
                            *p = percent;
                        }
                    }
                }
                Event::Scanned(_, Ok(scan)) => {
                    let root = scan.read().map(|s| s.tree.root()).unwrap_or(0);
                    self.scan = Some(scan);
                    self.phase = Phase::Ready;
                    self.expanded = HashSet::from([root]);
                    self.rows_for = None;
                    self.selected = None;
                    self.focus = root;
                    self.root = root;
                    self.layout = None;
                    self.largest = None;
                    self.kinds = None;
                    // Found in the old index; its node ids mean nothing now.
                    self.dupes.stop();
                    let min_size = self.dupes.min_size;
                    self.dupes = DupesView::new();
                    self.dupes.min_size = min_size;
                    self.cleanup.list.clear();
                    self.cleanup.generation = None;
                    self.changes = ChangesView::new();
                    self.scanned_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    self.worker.send(Request::Drives);
                    // What was saved before this scan; this scan's own record
                    // follows once it is written.
                    self.worker.send(Request::ListSnapshots(self.letter));
                }
                Event::Scanned(letter, Err(message)) => {
                    self.phase = if message == "needs_elevation" {
                        Phase::NeedsElevation
                    } else {
                        Phase::Failed { letter, message }
                    };
                }
                Event::Changed => self.after_change(),
                Event::Snapshots(saved) => {
                    if self.changes.chosen.is_none() {
                        // Compare with the last scan before this one; if this is
                        // the first, with this scan itself, which still shows what
                        // changed live since.
                        let before = saved
                            .iter()
                            .find(|s| s.taken + 60 < self.scanned_at)
                            .or(saved.first());
                        self.changes.chosen = before.map(|s| s.path.clone());
                    }
                    self.changes.saved = saved;
                }
                Event::Compared {
                    saved,
                    used_now,
                    changes,
                } => {
                    self.changes.computing = false;
                    self.changes.result = Some((saved, used_now, changes));
                }
                Event::Suggestions { generation, list } => {
                    self.cleanup.computing = false;
                    self.cleanup.generation = Some(generation);
                    if !self.cleanup.primed {
                        // Tick what cannot hurt; the rest waits for a decision.
                        self.cleanup.chosen = list
                            .iter()
                            .filter(|s| {
                                s.rule.safety == cleanup::Safety::Safe
                                    && s.rule.mode != cleanup::Mode::Info
                            })
                            .map(|s| s.rule.id)
                            .collect();
                        self.cleanup.primed = true;
                    }
                    self.cleanup.list = list;
                }
                Event::Recycled { result, bytes } => {
                    self.recycling = false;
                    // Whatever went is no longer a duplicate of anything.
                    self.dupes
                        .chosen
                        .retain(|p| std::fs::symlink_metadata(p).is_ok());
                    for group in &mut self.dupes.groups {
                        group
                            .files
                            .retain(|f| std::fs::symlink_metadata(&f.path).is_ok());
                    }
                    self.dupes.groups.retain(|g| g.files.len() > 1);
                    self.dupes.relayout();
                    // Ask the rules again once the watcher has caught up.
                    self.cleanup.generation = None;
                    self.cleanup.asked = None;
                    let text = self.lang.recycled(
                        result.gone,
                        result.requested,
                        &format::size(self.lang, bytes),
                    );
                    self.inform(text);
                }
                Event::DupesProgress(progress) => {
                    if self.dupes.cancel.is_some() {
                        self.dupes.progress = Some(progress);
                    }
                }
                Event::DupesFound {
                    groups,
                    seconds,
                    cancelled,
                } => {
                    self.dupes.cancel = None;
                    self.dupes.progress = None;
                    self.dupes.groups = groups;
                    self.dupes.finished = Some((seconds, cancelled));
                    // Open the first few; a thousand open groups is a wall.
                    self.dupes.collapsed = (10..self.dupes.groups.len()).collect();
                    self.dupes.relayout();
                }
                Event::Stale => {
                    let text = self.lang.strings().stale.to_string();
                    self.inform(text);
                }
                Event::Failed(message) => {
                    let text = self.lang.error(&message);
                    self.inform(text);
                }
                Event::Restarting => {}
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let mut keys = Vec::new();
        ctx.input_mut(|input| {
            use egui::{Key as K, Modifiers as M};
            for key in [
                K::F5,
                K::Backspace,
                K::ArrowUp,
                K::ArrowDown,
                K::ArrowLeft,
                K::ArrowRight,
            ] {
                if (!typing || key == K::F5) && input.consume_key(M::NONE, key) {
                    keys.push(key);
                }
            }
        });

        let Some(shared) = self.scan.clone() else {
            if keys.contains(&egui::Key::F5) && !matches!(self.phase, Phase::Scanning { .. }) {
                self.start_scan();
            }
            return;
        };
        let scan = shared.read().expect("scan lock");
        let tree = &scan.tree;

        for key in keys {
            match key {
                egui::Key::F5 => {
                    drop(scan);
                    self.start_scan();
                    return;
                }
                egui::Key::Backspace => {
                    if let Some(parent) = tree.parent(self.focus) {
                        self.focus = parent;
                    }
                }
                egui::Key::ArrowUp | egui::Key::ArrowDown if self.tab == Tab::Folders => {
                    let at = self
                        .selected
                        .and_then(|s| self.rows.iter().position(|r| r.node == s));
                    let next = match (key, at) {
                        (egui::Key::ArrowDown, Some(i)) => {
                            (i + 1).min(self.rows.len().saturating_sub(1))
                        }
                        (egui::Key::ArrowUp, Some(i)) => i.saturating_sub(1),
                        _ => 0,
                    };
                    if let Some(row) = self.rows.get(next) {
                        self.selected = Some(row.node);
                        self.scroll_to_row = true;
                    }
                }
                egui::Key::ArrowRight if self.tab == Tab::Folders => {
                    if let Some(node) = self.selected.filter(|n| tree.is_dir(*n)) {
                        self.expanded.insert(node);
                        self.rows_for = None;
                    }
                }
                egui::Key::ArrowLeft if self.tab == Tab::Folders => {
                    if let Some(node) = self.selected {
                        if tree.is_dir(node) && self.expanded.remove(&node) {
                            self.rows_for = None;
                        } else if let Some(parent) = tree.parent(node) {
                            self.selected = Some(parent);
                            self.scroll_to_row = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn apply(&mut self, actions: Vec<Action>, scan: &Scan, ctx: &egui::Context) {
        let tree = &scan.tree;
        for action in actions {
            match action {
                Action::Select { node, from_map } => {
                    self.selected = Some(node);
                    if from_map {
                        rows::open_ancestors(tree, &mut self.expanded, node);
                        self.rows_for = None;
                        self.scroll_to_row = true;
                        self.tab = Tab::Folders;
                    } else if !tree.ancestry(node).contains(&self.focus) {
                        // Selected in the tree but outside the focused part of
                        // the map: widen the map so the tile can be seen.
                        self.focus = tree.root();
                    }
                }
                Action::Toggle(node) => {
                    if !self.expanded.remove(&node) {
                        self.expanded.insert(node);
                    }
                    self.rows_for = None;
                }
                Action::Focus(node) => {
                    if tree.is_dir(node) {
                        self.focus = node;
                    }
                }
                Action::Open(path) => self.worker.send(Request::Open(path)),
                Action::Reveal(path) => self.worker.send(Request::Reveal(path)),
                Action::Recycle { paths, bytes } => {
                    if !paths.is_empty() && !self.recycling {
                        self.confirm = Some(Confirm { paths, bytes });
                    }
                }
                Action::Copy(path) => {
                    ctx.copy_text(path);
                    self.inform(self.lang.strings().copied.to_string());
                }
            }
        }
    }
}

impl eframe::App for DiskApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, prefs::KEY, &self.prefs());
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let events = self.worker.drain();
        let restarting = events.iter().any(|e| matches!(e, Event::Restarting));
        self.absorb(events);
        if restarting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.handle_keys(ctx);
        self.drive_screenshot(ctx);

        if let Some((_, since)) = &self.toast {
            let left = Duration::from_secs(3).saturating_sub(since.elapsed());
            if left.is_zero() {
                self.toast = None;
            } else {
                ctx.request_repaint_after(left);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.toolbar(ui);
        let shared = self.scan.clone();
        let guard = shared.as_ref().map(|s| s.read().expect("scan lock"));
        let ready = matches!(self.phase, Phase::Ready);

        match guard.as_deref() {
            Some(scan) if ready => {
                self.summary(ui, scan);
                self.status_bar(ui, Some(scan));
                let mut actions = Vec::new();
                self.left_panel(ui, scan, &mut actions);
                self.map_panel(ui, scan, &mut actions);
                self.apply(actions, scan, ui.ctx());
                self.confirm_dialog(ui.ctx());
                self.refresh_title(ui.ctx(), Some(scan));
            }
            _ => {
                self.status_bar(ui, None);
                self.message(ui);
                self.refresh_title(ui.ctx(), None);
            }
        }
    }
}

/* -------------------------------------------------------------------- *
 * Chrome
 * -------------------------------------------------------------------- */

impl DiskApp {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let busy = matches!(self.phase, Phase::Scanning { .. });
        let lang = self.lang;

        egui::Panel::top("toolbar")
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Burrow")
                            .color(palette.accent)
                            .strong()
                            .size(15.0),
                    );
                    ui.add_space(8.0);

                    let mut letter = self.letter;
                    egui::ComboBox::from_id_salt("drive")
                        .width(220.0)
                        .selected_text(drive_label(
                            lang,
                            self.drives.iter().find(|d| d.letter == letter),
                            letter,
                        ))
                        .show_ui(ui, |ui| {
                            for drive in &self.drives {
                                let text = drive_label(lang, Some(drive), drive.letter);
                                ui.add_enabled_ui(drive.is_ntfs(), |ui| {
                                    ui.selectable_value(&mut letter, drive.letter, text)
                                        .on_disabled_hover_text(strings.not_ntfs);
                                });
                            }
                        })
                        .response
                        .on_hover_text(strings.drive_tooltip);
                    if letter != self.letter && !busy {
                        self.letter = letter;
                        if !matches!(self.phase, Phase::NeedsElevation) {
                            self.start_scan();
                        }
                    }

                    ui.add_space(8.0);
                    let before = self.metric;
                    ui.selectable_value(&mut self.metric, Metric::OnDisk, strings.metric_on_disk)
                        .on_hover_text(strings.metric_tooltip);
                    ui.selectable_value(&mut self.metric, Metric::Size, strings.metric_size)
                        .on_hover_text(strings.metric_tooltip);
                    if before != self.metric {
                        self.rows_for = None;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let icon = if self.theme == Theme::Dark {
                            "☀"
                        } else {
                            "☾"
                        };
                        if ui
                            .button(icon)
                            .on_hover_text(strings.theme_tooltip)
                            .clicked()
                        {
                            self.theme = self.theme.flipped();
                            theme::apply(ui.ctx(), self.theme);
                        }
                        if ui
                            .add_enabled(
                                !busy && !matches!(self.phase, Phase::NeedsElevation),
                                egui::Button::new("⟳"),
                            )
                            .on_hover_text(strings.rescan_tooltip)
                            .clicked()
                        {
                            self.start_scan();
                        }
                        let mut chosen = self.lang;
                        egui::ComboBox::from_id_salt("language")
                            .width(52.0)
                            .selected_text(chosen.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut chosen, Lang::Tr, Lang::Tr.label());
                                ui.selectable_value(&mut chosen, Lang::En, Lang::En.label());
                            })
                            .response
                            .on_hover_text(strings.language_tooltip);
                        self.lang = chosen;
                    });
                });
            });
    }

    /// The drive's space as one bar: in files, in no file, and free.
    fn summary(&mut self, ui: &mut egui::Ui, scan: &Scan) {
        let palette = theme::palette(self.theme);
        let lang = self.lang;
        let drive = &scan.drive;
        let in_files = scan
            .tree
            .totals(scan.tree.root())
            .allocated
            .min(drive.used());
        let unexplained = drive.used().saturating_sub(in_files);

        egui::Panel::top("summary")
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 12,
                        top: 0,
                        bottom: 10,
                    }),
            )
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 10.0),
                    egui::Sense::hover(),
                );
                let total = drive.total.max(1) as f32;
                let painter = ui.painter();
                painter.rect_filled(rect, 3.0, palette.panel_2);
                let files_w = rect.width() * in_files as f32 / total;
                let other_w = rect.width() * unexplained as f32 / total;
                let files = egui::Rect::from_min_size(rect.min, egui::vec2(files_w, rect.height()));
                painter.rect_filled(files, 3.0, palette.accent);
                if other_w >= 1.0 {
                    // Two points of surface between the segments.
                    let other = egui::Rect::from_min_size(
                        rect.min + egui::vec2(files_w + 2.0, 0.0),
                        egui::vec2((other_w - 2.0).max(1.0), rect.height()),
                    );
                    painter.rect_filled(other, 3.0, palette.muted);
                }

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let size = |b| format::size(lang, b);
                    ui.label(lang.volume_usage(
                        &size(drive.used()),
                        &size(drive.free),
                        &size(drive.total),
                    ));
                    if unexplained > 0 {
                        ui.label(egui::RichText::new("·").color(palette.muted));
                        ui.label(
                            egui::RichText::new(lang.unexplained(&size(unexplained)))
                                .color(palette.muted),
                        )
                        .on_hover_text(lang.strings().unexplained_tooltip);
                    }
                });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui, scan: Option<&Scan>) {
        let palette = theme::palette(self.theme);
        let lang = self.lang;
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::NONE
                    .fill(palette.panel)
                    .inner_margin(egui::Margin::symmetric(12, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(scan) = scan {
                        let stats = &scan.index.stats;
                        ui.label(
                            egui::RichText::new(lang.status_line(
                                scan.letter,
                                stats.files,
                                stats.dirs,
                                scan.seconds,
                                &format::size(lang, scan.memory_bytes() as u64),
                            ))
                            .small()
                            .color(palette.muted),
                        );
                        let strings = lang.strings();
                        if scan.live {
                            ui.label(egui::RichText::new("●").small().color(palette.accent))
                                .on_hover_text(strings.live_tooltip);
                            ui.label(
                                egui::RichText::new(lang.live(scan.changes))
                                    .small()
                                    .color(palette.muted),
                            )
                            .on_hover_text(strings.live_tooltip);
                        } else {
                            ui.label(
                                egui::RichText::new(strings.not_live)
                                    .small()
                                    .color(palette.muted),
                            )
                            .on_hover_text(strings.stale);
                        }
                    }
                    if let Some((message, _)) = &self.toast {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(message).small().color(palette.accent));
                        });
                    }
                });
            });
    }

    fn message(&mut self, ui: &mut egui::Ui) {
        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::NONE.fill(palette.bg))
            .show(ui, |ui| match &self.phase {
                Phase::Starting | Phase::Ready => waiting(ui, strings.preparing, "", None),
                Phase::Scanning { letter, percent } => waiting(
                    ui,
                    &self.lang.scanning(*letter),
                    strings.scanning_detail,
                    Some(*percent),
                ),
                Phase::NoVolume => {
                    card(ui, strings.no_volume_title, strings.no_volume_detail, None);
                }
                Phase::NeedsElevation => {
                    if card(
                        ui,
                        strings.elevation_title,
                        strings.elevation_detail,
                        Some(strings.elevation_action),
                    ) {
                        self.worker.send(Request::RestartElevated);
                    }
                }
                Phase::Failed { letter, message } => {
                    let letter = *letter;
                    let detail = self.lang.error(message);
                    if card(ui, strings.scan_failed, &detail, Some(strings.retry)) {
                        self.letter = letter;
                        self.start_scan();
                    }
                }
            });
    }

    fn refresh_title(&mut self, ctx: &egui::Context, scan: Option<&Scan>) {
        let wanted = match scan {
            Some(scan) => self
                .lang
                .title(scan.letter, &format::size(self.lang, scan.drive.used())),
            None => "Burrow".to_string(),
        };
        if wanted != self.title {
            self.title = wanted.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(wanted));
        }
    }
}

fn drive_label(lang: Lang, drive: Option<&Drive>, letter: char) -> String {
    match drive {
        Some(d) if d.total > 0 => {
            let name = if d.label.is_empty() {
                String::new()
            } else {
                format!(" {}", d.label)
            };
            format!(
                "{letter}:{name}  —  {} / {}",
                format::size(lang, d.used()),
                format::size(lang, d.total)
            )
        }
        _ => format!("{letter}:"),
    }
}

/* -------------------------------------------------------------------- *
 * The left panel: tree, largest files, kinds
 * -------------------------------------------------------------------- */

impl DiskApp {
    fn left_panel(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let width = ui.available_width();

        egui::Panel::left("lists")
            .resizable(true)
            .default_size(width * 0.46)
            .size_range(360.0..=width * 0.75)
            .frame(
                egui::Frame::NONE
                    .fill(palette.bg)
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.tab, Tab::Folders, strings.tab_folders);
                    ui.selectable_value(&mut self.tab, Tab::Largest, strings.tab_largest);
                    ui.selectable_value(&mut self.tab, Tab::Kinds, strings.tab_kinds);
                    ui.selectable_value(&mut self.tab, Tab::Duplicates, strings.tab_duplicates);
                    ui.selectable_value(&mut self.tab, Tab::Cleanup, self.lang.words().tab_cleanup);
                    ui.selectable_value(&mut self.tab, Tab::Changes, self.lang.words().tab_changes);
                });
                ui.add_space(4.0);
                match self.tab {
                    Tab::Folders => self.folders(ui, scan, actions),
                    Tab::Largest => self.largest(ui, scan, actions),
                    Tab::Kinds => self.kinds(ui, scan),
                    Tab::Duplicates => self.duplicates(ui, scan, actions),
                    Tab::Cleanup => self.cleanup_tab(ui, scan, actions),
                    Tab::Changes => self.changes_tab(ui, scan, actions),
                }
            });
    }

    fn folders(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let key = (scan.generation, self.metric);
        if self.rows_for != Some(key) {
            self.rows = rows::flatten(&scan.tree, &self.expanded, self.metric);
            self.rows_for = Some(key);
        }
        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let lang = self.lang;
        let metric = self.metric;
        let tree = &scan.tree;
        let index = &scan.index;

        let mut builder = TableBuilder::new(ui)
            .id_salt("folders")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::initial(92.0).at_least(60.0).clip(true))
            .column(Column::initial(76.0).at_least(56.0).clip(true))
            .column(Column::initial(72.0).at_least(48.0).clip(true))
            .column(Column::initial(84.0).at_least(60.0).clip(true))
            .min_scrolled_height(0.0);
        if std::mem::take(&mut self.scroll_to_row) {
            if let Some(i) = self
                .selected
                .and_then(|s| self.rows.iter().position(|r| r.node == s))
            {
                builder = builder.scroll_to_row(i, Some(egui::Align::Center));
            }
        }

        let rows = &self.rows;
        let expanded = &self.expanded;
        let selected = self.selected;
        builder
            .header(24.0, |mut header| {
                for (label, right) in [
                    (strings.col_name, false),
                    (strings.col_share, false),
                    (strings.col_size, true),
                    (strings.col_files, true),
                    (strings.col_modified, false),
                ] {
                    header.col(|ui| heading(ui, label, right, palette));
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, rows.len(), |mut table_row| {
                    let row = rows[table_row.index()];
                    let node = row.node;
                    let totals = tree.totals(node);
                    let is_dir = tree.is_dir(node);
                    table_row.set_selected(selected == Some(node));

                    table_row.col(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        if is_dir && !tree.children(node).is_empty() {
                            let open = expanded.contains(&node);
                            let arrow = if open { "⏷" } else { "⏵" };
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(arrow).color(palette.muted),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                actions.push(Action::Toggle(node));
                            }
                        } else {
                            ui.add_space(18.0);
                        }
                        swatch(ui, self.theme, is_dir, index, node, tree.root());
                        let name = tree.name(index, node);
                        let text = if is_dir {
                            egui::RichText::new(name).strong()
                        } else {
                            egui::RichText::new(name)
                        };
                        ui.add(egui::Label::new(text).selectable(false).truncate());
                    });
                    table_row.col(|ui| {
                        // The root has no parent; its share is of the used space.
                        let parent = match tree.parent(node) {
                            Some(p) => value(tree, p, metric),
                            None => scan.drive.used(),
                        };
                        share_bar(ui, lang, value(tree, node, metric), parent, palette);
                    });
                    table_row.col(|ui| {
                        right_label(
                            ui,
                            &format::size(lang, value(tree, node, metric)),
                            palette.text,
                        );
                    });
                    table_row.col(|ui| {
                        if is_dir {
                            right_label(ui, &lang.number(totals.files as u64), palette.muted);
                        }
                    });
                    table_row.col(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format::date(totals.newest))
                                    .color(palette.muted),
                            )
                            .selectable(false),
                        );
                    });

                    let response = table_row.response();
                    if response.clicked() {
                        actions.push(Action::Select {
                            node,
                            from_map: false,
                        });
                    }
                    if response.double_clicked() && is_dir {
                        actions.push(Action::Toggle(node));
                    }
                    response.context_menu(|ui| {
                        node_menu(ui, lang, scan, node, actions);
                    });
                });
            });
    }

    fn largest(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let metric = self.metric;
        let fresh =
            matches!(&self.largest, Some((g, m, _)) if *g == scan.generation && *m == metric);
        if !fresh {
            let mut list = scan.tree.largest_files(1000);
            list.sort_by_key(|n| std::cmp::Reverse(value(&scan.tree, *n, metric)));
            self.largest = Some((scan.generation, metric, list));
        }
        let Some((_, _, list)) = &self.largest else {
            return;
        };

        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let lang = self.lang;
        let tree = &scan.tree;
        let index = &scan.index;
        let used = scan.drive.used();
        let selected = self.selected;
        let theme = self.theme;

        TableBuilder::new(ui)
            .id_salt("largest")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(220.0).at_least(120.0).clip(true))
            .column(Column::initial(76.0).at_least(56.0).clip(true))
            .column(Column::initial(92.0).at_least(60.0).clip(true))
            .column(Column::remainder().at_least(120.0).clip(true))
            .min_scrolled_height(0.0)
            .header(24.0, |mut header| {
                for (label, right) in [
                    (strings.col_name, false),
                    (strings.col_size, true),
                    (strings.col_share, false),
                    (strings.col_folder, false),
                ] {
                    header.col(|ui| heading(ui, label, right, palette));
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, list.len(), |mut table_row| {
                    let node = list[table_row.index()];
                    table_row.set_selected(selected == Some(node));
                    table_row.col(|ui| {
                        swatch(ui, theme, false, index, node, tree.root());
                        ui.add(
                            egui::Label::new(index.name(node as usize))
                                .selectable(false)
                                .truncate(),
                        );
                    });
                    table_row.col(|ui| {
                        right_label(
                            ui,
                            &format::size(lang, value(tree, node, metric)),
                            palette.text,
                        )
                    });
                    table_row
                        .col(|ui| share_bar(ui, lang, tree.totals(node).allocated, used, palette));
                    table_row.col(|ui| {
                        let folder = index.parent_path(node as usize).unwrap_or_default();
                        ui.add(
                            egui::Label::new(egui::RichText::new(folder).color(palette.muted))
                                .selectable(false)
                                .truncate(),
                        );
                    });
                    let response = table_row.response();
                    if response.clicked() {
                        actions.push(Action::Select {
                            node,
                            from_map: false,
                        });
                    }
                    response.context_menu(|ui| node_menu(ui, lang, scan, node, actions));
                });
            });
    }

    fn kinds(&mut self, ui: &mut egui::Ui, scan: &Scan) {
        let focus = self.focus;
        let fresh =
            matches!(&self.kinds, Some((g, f, _, _)) if *g == scan.generation && *f == focus);
        if !fresh {
            let kinds = scan.tree.kinds_under(&scan.index, focus);
            let mut exts = scan.tree.extensions_under(&scan.index, focus);
            exts.truncate(200);
            self.kinds = Some((scan.generation, focus, kinds, exts));
        }
        let Some((_, _, kinds, exts)) = &self.kinds else {
            return;
        };

        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let lang = self.lang;
        let whole = value(&scan.tree, focus, Metric::OnDisk);

        ui.label(
            egui::RichText::new(scan.tree.path(&scan.index, focus))
                .color(palette.muted)
                .small(),
        );
        ui.add_space(4.0);

        TableBuilder::new(ui)
            .id_salt("kinds")
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(140.0).clip(true))
            .column(Column::initial(92.0).at_least(60.0))
            .column(Column::initial(80.0).at_least(56.0))
            .column(Column::initial(80.0).at_least(48.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(ROW_HEIGHT * 14.0 + 24.0)
            .header(24.0, |mut header| {
                for (label, right) in [
                    (strings.col_kind, false),
                    (strings.col_share, false),
                    (strings.col_size, true),
                    (strings.col_files, true),
                ] {
                    header.col(|ui| heading(ui, label, right, palette));
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, kinds.len(), |mut row| {
                    let (kind, totals) = kinds[row.index()];
                    row.col(|ui| {
                        colour_chip(ui, theme::kind_colour(self.theme, kind));
                        ui.label(lang.kind(kind));
                    });
                    row.col(|ui| share_bar(ui, lang, totals.allocated, whole, palette));
                    row.col(|ui| {
                        right_label(ui, &format::size(lang, totals.allocated), palette.text)
                    });
                    row.col(|ui| right_label(ui, &lang.number(totals.files as u64), palette.muted));
                });
            });

        ui.add_space(10.0);
        TableBuilder::new(ui)
            .id_salt("extensions")
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(140.0).clip(true))
            .column(Column::initial(92.0).at_least(60.0))
            .column(Column::initial(80.0).at_least(56.0))
            .column(Column::initial(80.0).at_least(48.0))
            .min_scrolled_height(0.0)
            .header(24.0, |mut header| {
                for (label, right) in [
                    (strings.col_extension, false),
                    (strings.col_share, false),
                    (strings.col_size, true),
                    (strings.col_files, true),
                ] {
                    header.col(|ui| heading(ui, label, right, palette));
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, exts.len(), |mut row| {
                    let (ext, totals) = &exts[row.index()];
                    row.col(|ui| {
                        let kind = Kind::of(&format!("x.{ext}"));
                        colour_chip(ui, theme::kind_colour(self.theme, kind));
                        if ext.is_empty() {
                            ui.label(
                                egui::RichText::new(strings.no_extension).color(palette.muted),
                            );
                        } else {
                            ui.label(format!(".{ext}"));
                        }
                    });
                    row.col(|ui| share_bar(ui, lang, totals.allocated, whole, palette));
                    row.col(|ui| {
                        right_label(ui, &format::size(lang, totals.allocated), palette.text)
                    });
                    row.col(|ui| right_label(ui, &lang.number(totals.files as u64), palette.muted));
                });
            });
    }
}

impl DiskApp {
    fn duplicates(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let lang = self.lang;
        let running = self.dupes.cancel.is_some();

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(strings.dupes_min).color(palette.muted));
            for size in [1u64 << 20, 10 << 20, 100 << 20] {
                ui.add_enabled_ui(!running, |ui| {
                    ui.selectable_value(&mut self.dupes.min_size, size, format::size(lang, size));
                });
            }
            ui.add_space(8.0);
            if running {
                if ui.button(strings.dupes_stop).clicked() {
                    self.dupes.stop();
                }
            } else if ui.button(strings.dupes_start).clicked() {
                let cancel = Arc::new(AtomicBool::new(false));
                self.dupes.cancel = Some(cancel.clone());
                self.dupes.progress = None;
                if let Some(shared) = self.scan.clone() {
                    self.worker.send(Request::FindDuplicates {
                        scan: shared,
                        min_size: self.dupes.min_size,
                        cancel,
                    });
                }
            }
        });
        ui.add_space(4.0);

        if running {
            let p = self.dupes.progress.unwrap_or_default();
            let fraction = p.bytes_done as f32 / p.bytes_total.max(1) as f32;
            ui.add(
                egui::ProgressBar::new(fraction)
                    .corner_radius(4)
                    .desired_height(8.0),
            );
            ui.label(
                egui::RichText::new(lang.dupes_progress(
                    &format::size(lang, p.bytes_done),
                    &format::size(lang, p.bytes_total),
                    p.groups,
                ))
                .small()
                .color(palette.muted),
            );
            ui.add_space(4.0);
        } else {
            match self.dupes.finished {
                None => {
                    ui.label(egui::RichText::new(strings.dupes_intro).color(palette.muted));
                    return;
                }
                Some((seconds, cancelled)) => {
                    let wasted: u64 = self.dupes.groups.iter().map(|g| g.wasted()).sum();
                    let text = if self.dupes.groups.is_empty() {
                        strings.dupes_none.to_string()
                    } else {
                        lang.dupes_summary(
                            self.dupes.groups.len(),
                            &format::size(lang, wasted),
                            seconds,
                        )
                    };
                    ui.label(egui::RichText::new(text).strong());
                    if cancelled {
                        ui.label(
                            egui::RichText::new(strings.dupes_cancelled)
                                .small()
                                .color(palette.muted),
                        );
                    }
                    ui.add_space(4.0);
                }
            }
        }

        // Removal: tick copies by hand, or keep one per group and tick the rest.
        let protected = |node: NodeId| {
            !scan.tree.contains(node) || cleanup::is_protected(&scan.index, &scan.tree, node)
        };
        if !running && !self.dupes.groups.is_empty() {
            let words = lang.words();
            ui.horizontal(|ui| {
                if ui.small_button(words.keep_one).clicked() {
                    self.dupes.chosen.clear();
                    for group in &self.dupes.groups {
                        // Keep the copy with the shortest path — usually the
                        // original, the others being copies made into deeper
                        // folders.
                        let keep = group
                            .files
                            .iter()
                            .min_by_key(|f| (f.path.len(), f.path.clone()));
                        for file in &group.files {
                            if Some(file) != keep && !protected(file.node) {
                                self.dupes.chosen.insert(file.path.clone());
                            }
                        }
                    }
                }
                if !self.dupes.chosen.is_empty() && ui.small_button(words.clear_selection).clicked()
                {
                    self.dupes.chosen.clear();
                }
            });
            let chosen: Vec<&dupes::Candidate> = self
                .dupes
                .groups
                .iter()
                .flat_map(|g| g.files.iter())
                .filter(|f| self.dupes.chosen.contains(&f.path))
                .collect();
            if !chosen.is_empty() {
                let bytes: u64 = chosen
                    .iter()
                    .map(|f| scan.tree.totals(f.node).allocated)
                    .sum();
                let label = format!(
                    "{}  ({})",
                    words.recycle_selected,
                    lang.recycle_count(chosen.len(), &format::size(lang, bytes))
                );
                if ui
                    .add_enabled(!self.recycling, egui::Button::new(label))
                    .clicked()
                {
                    actions.push(Action::Recycle {
                        paths: chosen.iter().map(|f| f.path.clone()).collect(),
                        bytes,
                    });
                }
            }
            ui.add_space(4.0);
        }

        let groups = &self.dupes.groups;
        let rows = &self.dupes.rows;
        let selected = self.selected;
        let mut toggled = None;
        let mut ticked: Option<(String, bool)> = None;
        TableBuilder::new(ui)
            .id_salt("duplicates")
            .striped(false)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::initial(96.0).at_least(64.0))
            .min_scrolled_height(0.0)
            .body(|body| {
                body.rows(ROW_HEIGHT, rows.len(), |mut table_row| {
                    match rows[table_row.index()] {
                        DupRow::Group(g) => {
                            let group = &groups[g];
                            let open = !self.dupes.collapsed.contains(&g);
                            table_row.col(|ui| {
                                ui.label(
                                    egui::RichText::new(if open { "⏷" } else { "⏵" })
                                        .color(palette.muted),
                                );
                                ui.label(
                                    egui::RichText::new(lang.dupes_group(
                                        group.files.len(),
                                        &format::size(lang, group.size),
                                    ))
                                    .strong(),
                                );
                            });
                            table_row.col(|ui| {
                                right_label(
                                    ui,
                                    &format::size(lang, group.wasted()),
                                    palette.danger,
                                );
                            });
                            if table_row.response().clicked() {
                                toggled = Some(g);
                            }
                        }
                        DupRow::File(g, f) => {
                            let file = &groups[g].files[f];
                            table_row.set_selected(selected == Some(file.node));
                            let (folder, name) =
                                file.path.rsplit_once('\\').unwrap_or(("", &file.path));
                            table_row.col(|ui| {
                                // Protected places are no longer searched, but a
                                // result can outlive a change on disk.
                                if protected(file.node) {
                                    padlock(ui, palette.muted)
                                        .on_hover_text(lang.words().protected);
                                } else {
                                    let mut on = self.dupes.chosen.contains(&file.path);
                                    if ui.checkbox(&mut on, "").changed() {
                                        ticked = Some((file.path.clone(), on));
                                    }
                                }
                                swatch(
                                    ui,
                                    self.theme,
                                    false,
                                    &scan.index,
                                    file.node,
                                    scan.tree.root(),
                                );
                                ui.add(egui::Label::new(name).selectable(false));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(folder).small().color(palette.muted),
                                    )
                                    .selectable(false)
                                    .truncate(),
                                );
                            });
                            table_row.col(|_| {});
                            let response = table_row.response();
                            if response.clicked() && scan.tree.contains(file.node) {
                                actions.push(Action::Select {
                                    node: file.node,
                                    from_map: false,
                                });
                            }
                            response.context_menu(|ui| {
                                ui.set_min_width(200.0);
                                if ui.button(strings.menu_open).clicked() {
                                    actions.push(Action::Open(file.path.clone()));
                                    ui.close();
                                }
                                if ui.button(strings.menu_reveal).clicked() {
                                    actions.push(Action::Reveal(file.path.clone()));
                                    ui.close();
                                }
                                if ui.button(strings.menu_copy).clicked() {
                                    actions.push(Action::Copy(file.path.clone()));
                                    ui.close();
                                }
                            });
                        }
                    }
                });
            });
        if let Some((path, on)) = ticked {
            if on {
                self.dupes.chosen.insert(path);
            } else {
                self.dupes.chosen.remove(&path);
            }
        }
        if let Some(g) = toggled {
            if !self.dupes.collapsed.remove(&g) {
                self.dupes.collapsed.insert(g);
            }
            self.dupes.relayout();
        }
    }
}

impl DiskApp {
    fn cleanup_tab(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let palette = theme::palette(self.theme);
        let lang = self.lang;
        let words = lang.words();

        // The rules walk the whole tree, so they run on the worker, and at
        // most every few seconds while the disk is changing underneath.
        let stale = self.cleanup.generation != Some(scan.generation);
        let rested = self
            .cleanup
            .asked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(8));
        if stale && rested && !self.cleanup.computing {
            if let Some(shared) = self.scan.clone() {
                self.cleanup.computing = true;
                self.cleanup.asked = Some(Instant::now());
                self.worker.send(Request::Suggest(shared));
            }
        }

        ui.label(egui::RichText::new(words.cleanup_intro).color(palette.muted));
        ui.add_space(6.0);
        if self.cleanup.generation.is_none() {
            waiting(ui, words.computing, "", None);
            return;
        }
        if self.cleanup.list.is_empty() {
            ui.label(words.nothing_found);
            return;
        }

        // What the ticked rules would remove.
        let chosen: Vec<&cleanup::Suggestion> = self
            .cleanup
            .list
            .iter()
            .filter(|s| {
                s.rule.mode != cleanup::Mode::Info && self.cleanup.chosen.contains(s.rule.id)
            })
            .collect();
        let chosen_bytes: u64 = chosen.iter().map(|s| s.bytes).sum();
        ui.horizontal(|ui| {
            let enabled = !chosen.is_empty() && !self.recycling;
            let label = format!(
                "{}  ({})",
                words.recycle_selected,
                format::size(lang, chosen_bytes)
            );
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                let paths = removal_paths(scan, &chosen);
                actions.push(Action::Recycle {
                    paths,
                    bytes: chosen_bytes,
                });
            }
            if self.recycling {
                ui.spinner();
                ui.label(egui::RichText::new(words.recycling).color(palette.muted));
            }
        });
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for safety in [
                    cleanup::Safety::Safe,
                    cleanup::Safety::Likely,
                    cleanup::Safety::Careful,
                ] {
                    let in_band: Vec<&cleanup::Suggestion> = self
                        .cleanup
                        .list
                        .iter()
                        .filter(|s| s.rule.safety == safety)
                        .collect();
                    if in_band.is_empty() {
                        continue;
                    }
                    let (title, note) = lang.safety(safety);
                    let colour = match safety {
                        cleanup::Safety::Safe => palette.accent,
                        cleanup::Safety::Likely => palette.text,
                        cleanup::Safety::Careful => palette.danger,
                    };
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(title).strong().color(colour));
                    ui.label(egui::RichText::new(note).small().color(palette.muted));
                    ui.add_space(2.0);

                    for suggestion in in_band {
                        let id = suggestion.rule.id;
                        let (name, explanation) = lang.rule(id);
                        egui::Frame::NONE
                            .fill(palette.panel)
                            .corner_radius(6)
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    if suggestion.rule.mode == cleanup::Mode::Info {
                                        ui.add_space(22.0);
                                    } else {
                                        let mut on = self.cleanup.chosen.contains(id);
                                        if ui.checkbox(&mut on, "").changed() {
                                            if on {
                                                self.cleanup.chosen.insert(id);
                                            } else {
                                                self.cleanup.chosen.remove(id);
                                            }
                                        }
                                    }
                                    ui.label(egui::RichText::new(name).strong());
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(format::size(
                                                    lang,
                                                    suggestion.bytes,
                                                ))
                                                .strong(),
                                            );
                                        },
                                    );
                                });
                                ui.label(
                                    egui::RichText::new(explanation)
                                        .small()
                                        .color(palette.muted),
                                );
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(lang.files(suggestion.files as u64))
                                            .small()
                                            .color(palette.muted),
                                    );
                                    match id {
                                        "recycle_bin" => {
                                            if ui.small_button(words.open_recycle_bin).clicked() {
                                                self.worker.send(Request::OpenRecycleBin);
                                            }
                                        }
                                        "windows_old" => {
                                            if ui.small_button(words.open_disk_cleanup).clicked() {
                                                self.worker.send(Request::OpenDiskCleanup);
                                            }
                                        }
                                        "hibernation" => {
                                            if ui.small_button(words.copy_command).clicked() {
                                                ui.ctx().copy_text("powercfg /h off".into());
                                                self.toast = Some((
                                                    words.command_copied.to_string(),
                                                    Instant::now(),
                                                ));
                                            }
                                        }
                                        _ => {
                                            let open = self.cleanup.open.contains(id);
                                            let arrow = if open { "⏷" } else { "⏵" };
                                            if ui
                                                .small_button(format!(
                                                    "{arrow} {}",
                                                    words.show_items
                                                ))
                                                .clicked()
                                            {
                                                if open {
                                                    self.cleanup.open.remove(id);
                                                } else {
                                                    self.cleanup.open.insert(id);
                                                }
                                            }
                                        }
                                    }
                                });
                                if self.cleanup.open.contains(id) {
                                    for &target in suggestion.targets.iter().take(50) {
                                        if !scan.tree.contains(target) {
                                            continue;
                                        }
                                        ui.horizontal(|ui| {
                                            ui.add_space(22.0);
                                            let path = scan.tree.path(&scan.index, target);
                                            let size = format::size(
                                                lang,
                                                scan.tree.totals(target).allocated,
                                            );
                                            let response = ui.add(
                                                egui::Label::new(egui::RichText::new(path).small())
                                                    .truncate()
                                                    .sense(egui::Sense::click()),
                                            );
                                            if response.clicked() {
                                                actions.push(Action::Select {
                                                    node: target,
                                                    from_map: false,
                                                });
                                            }
                                            response.context_menu(|ui| {
                                                node_menu(ui, lang, scan, target, actions)
                                            });
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        egui::RichText::new(size)
                                                            .small()
                                                            .color(palette.muted),
                                                    );
                                                },
                                            );
                                        });
                                    }
                                    if suggestion.targets.len() > 50 {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "… +{}",
                                                suggestion.targets.len() - 50
                                            ))
                                            .small()
                                            .color(palette.muted),
                                        );
                                    }
                                }
                            });
                        ui.add_space(4.0);
                    }
                }
            });
    }

    /// The "are you sure" for any removal, from anywhere in the window.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else { return };
        let lang = self.lang;
        let words = lang.words();
        let palette = theme::palette(self.theme);
        let mut decided: Option<bool> = None;

        let modal = egui::Modal::new(egui::Id::new("confirm-recycle")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.label(egui::RichText::new(words.confirm_title).heading());
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    lang.recycle_count(confirm.paths.len(), &format::size(lang, confirm.bytes)),
                )
                .strong(),
            );
            ui.add_space(4.0);
            for path in confirm.paths.iter().take(6) {
                ui.add(
                    egui::Label::new(egui::RichText::new(path).small().color(palette.muted))
                        .truncate(),
                );
            }
            if confirm.paths.len() > 6 {
                ui.label(
                    egui::RichText::new(format!("… +{}", confirm.paths.len() - 6))
                        .small()
                        .color(palette.muted),
                );
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new(words.confirm_detail).small());
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .button(egui::RichText::new(words.confirm_action).strong())
                    .clicked()
                {
                    decided = Some(true);
                }
                if ui.button(words.cancel).clicked() {
                    decided = Some(false);
                }
            });
        });
        if modal.should_close() && decided.is_none() {
            decided = Some(false);
        }
        match decided {
            Some(true) => {
                if let Some(confirm) = self.confirm.take() {
                    self.recycling = true;
                    self.worker.send(Request::Recycle {
                        paths: confirm.paths,
                        bytes: confirm.bytes,
                    });
                }
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }
}

impl DiskApp {
    fn changes_tab(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let palette = theme::palette(self.theme);
        let lang = self.lang;
        let words = lang.words();

        ui.label(egui::RichText::new(words.changes_intro).color(palette.muted));
        ui.add_space(6.0);
        if self.changes.saved.is_empty() {
            ui.label(words.first_snapshot);
            return;
        }

        // Which snapshot to compare with.
        let mut chosen = self.changes.chosen.clone();
        let label = |s: &snapshots::Saved| {
            format!(
                "{}  ·  {} {}",
                shell::local_time(s.taken),
                format::size(lang, s.used),
                lang.strings().used
            )
        };
        let current = self
            .changes
            .saved
            .iter()
            .find(|s| Some(&s.path) == chosen.as_ref())
            .map(label)
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(words.compare_with).color(palette.muted));
            egui::ComboBox::from_id_salt("snapshot")
                .width(300.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for s in &self.changes.saved {
                        ui.selectable_value(&mut chosen, Some(s.path.clone()), label(s));
                    }
                });
        });
        if chosen != self.changes.chosen {
            self.changes.chosen = chosen;
            self.changes.result = None;
            self.changes.computed_for = None;
            self.changes.asked = None;
        }

        // Compare on the worker; again when the disk has moved on, but not
        // more often than every few seconds.
        let Some(path) = self.changes.chosen.clone() else {
            return;
        };
        let want = (scan.generation, path.clone());
        let rested = self
            .changes
            .asked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(8));
        if self.changes.computed_for.as_ref() != Some(&want) && rested && !self.changes.computing {
            if let (Some(shared), Some(saved)) = (
                self.scan.clone(),
                self.changes.saved.iter().find(|s| s.path == path).cloned(),
            ) {
                self.changes.computing = true;
                self.changes.asked = Some(Instant::now());
                self.changes.computed_for = Some(want);
                self.worker.send(Request::Compare {
                    scan: shared,
                    saved,
                });
            }
        }

        let Some((saved, used_now, list)) = &self.changes.result else {
            waiting(ui, words.computing, "", None);
            return;
        };

        let delta = *used_now as i64 - saved.used as i64;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} → {}",
                format::size(lang, saved.used),
                format::size(lang, *used_now)
            ));
            ui.label(
                egui::RichText::new(signed(lang, delta))
                    .strong()
                    .color(change_colour(palette, delta)),
            );
            ui.label(egui::RichText::new(words.used_space_change).color(palette.muted));
        });
        ui.add_space(6.0);
        if list.is_empty() {
            ui.label(egui::RichText::new(words.no_changes).color(palette.muted));
            return;
        }

        let largest = list
            .iter()
            .map(|c| c.delta().unsigned_abs())
            .max()
            .unwrap_or(1);
        let selected = self.selected;
        TableBuilder::new(ui)
            .id_salt("changes")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(96.0).at_least(80.0))
            .column(Column::initial(90.0).at_least(40.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::initial(150.0).at_least(100.0).clip(true))
            .min_scrolled_height(0.0)
            .header(24.0, |mut header| {
                let strings = lang.strings();
                for (label, right) in [
                    (words.col_change, true),
                    ("", false),
                    (strings.col_folder, false),
                    (words.col_then_now, false),
                ] {
                    header.col(|ui| heading(ui, label, right, palette));
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, list.len(), |mut row| {
                    let change = &list[row.index()];
                    let d = change.delta();
                    let node = snapshot::find(&scan.index, &scan.tree, &change.path);
                    row.set_selected(node.is_some() && node == selected);
                    row.col(|ui| right_label(ui, &signed(lang, d), change_colour(palette, d)));
                    row.col(|ui| {
                        let width = ui.available_width().min(80.0);
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
                        let share = d.unsigned_abs() as f32 / largest as f32;
                        let bar = egui::Rect::from_min_size(
                            rect.min,
                            egui::vec2((width * share).max(2.0), 6.0),
                        );
                        ui.painter()
                            .rect_filled(bar, 3.0, change_colour(palette, d));
                    });
                    row.col(|ui| {
                        // The folder's own name first: a long path truncated
                        // from the right would hide exactly the part that
                        // says which folder it is.
                        let trimmed = change.path.trim_end_matches('\\');
                        let (parent, name) = match trimmed.rsplit_once('\\') {
                            Some((parent, name)) if !name.is_empty() => (parent, name),
                            _ => ("", change.path.as_str()),
                        };
                        let name = if node.is_some() {
                            egui::RichText::new(name).strong()
                        } else {
                            // Gone since: shown, but it cannot be selected.
                            egui::RichText::new(name)
                                .color(palette.muted)
                                .strikethrough()
                        };
                        ui.add(egui::Label::new(name).selectable(false));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(parent).small().color(palette.muted),
                            )
                            .selectable(false)
                            .truncate(),
                        )
                        .on_hover_text(&change.path);
                    });
                    row.col(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} → {}",
                                format::size(lang, change.before),
                                format::size(lang, change.after)
                            ))
                            .small()
                            .color(palette.muted),
                        );
                    });
                    let response = row.response();
                    if let Some(node) = node {
                        if response.clicked() {
                            actions.push(Action::Select {
                                node,
                                from_map: false,
                            });
                        }
                        response.context_menu(|ui| node_menu(ui, lang, scan, node, actions));
                    }
                });
            });
    }
}

/// `+8,2 GB` / `−1,1 GB`.
fn signed(lang: Lang, delta: i64) -> String {
    let sign = if delta >= 0 { "+" } else { "−" };
    format!("{sign}{}", format::size(lang, delta.unsigned_abs()))
}

/// Growth is what fills a disk, so it takes the warning colour; shrinkage
/// the accent.
fn change_colour(palette: &theme::Palette, delta: i64) -> egui::Color32 {
    if delta > 0 {
        palette.danger
    } else {
        palette.accent
    }
}

/// The paths to hand the recycle bin for a set of suggestions: a cache
/// folder's contents, or the matched thing itself.
fn removal_paths(scan: &Scan, chosen: &[&cleanup::Suggestion]) -> Vec<String> {
    let tree = &scan.tree;
    let mut paths = Vec::new();
    for suggestion in chosen {
        for &target in &suggestion.targets {
            if !tree.contains(target) {
                continue;
            }
            match suggestion.rule.mode {
                cleanup::Mode::Contents => {
                    paths.extend(
                        tree.children(target)
                            .iter()
                            .map(|c| tree.path(&scan.index, *c)),
                    );
                }
                cleanup::Mode::Itself => paths.push(tree.path(&scan.index, target)),
                cleanup::Mode::Info => {}
            }
        }
    }
    paths
}

/* -------------------------------------------------------------------- *
 * The map
 * -------------------------------------------------------------------- */

impl DiskApp {
    fn map_panel(&mut self, ui: &mut egui::Ui, scan: &Scan, actions: &mut Vec<Action>) {
        let palette = theme::palette(self.theme);
        let strings = self.lang.strings();
        let lang = self.lang;
        let tree = &scan.tree;
        let index = &scan.index;
        if !tree.contains(self.focus) || !tree.is_dir(self.focus) {
            self.focus = tree.root();
        }

        egui::CentralPanel::no_frame()
            .frame(
                egui::Frame::NONE
                    .fill(palette.bg)
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                // Where the map is focused, as a clickable path.
                ui.horizontal(|ui| {
                    let up = tree.parent(self.focus);
                    if ui
                        .add_enabled(up.is_some(), egui::Button::new(strings.map_up))
                        .clicked()
                    {
                        if let Some(parent) = up {
                            actions.push(Action::Focus(parent));
                        }
                    }
                    let chain = tree.ancestry(self.focus);
                    for (i, node) in chain.iter().enumerate() {
                        if i > 0 {
                            ui.label(egui::RichText::new("›").color(palette.muted));
                        }
                        let last = i + 1 == chain.len();
                        let text = egui::RichText::new(tree.name(index, *node));
                        let text = if last {
                            text.strong()
                        } else {
                            text.color(palette.muted)
                        };
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            actions.push(Action::Focus(*node));
                        }
                    }
                    ui.label(
                        egui::RichText::new(format::size(
                            lang,
                            value(tree, self.focus, self.metric),
                        ))
                        .color(palette.muted),
                    );
                });
                ui.add_space(4.0);

                // Leave room for the legend under the map.
                let size = ui.available_size() - egui::vec2(0.0, 58.0);
                let (rect, response) =
                    ui.allocate_exact_size(size.max(egui::vec2(10.0, 10.0)), egui::Sense::click());

                let key = treemap::Key {
                    generation: scan.generation,
                    focus: self.focus,
                    rect,
                    metric: self.metric,
                    theme: self.theme,
                };
                if self.layout.as_ref().map(|l| l.key) != Some(key) {
                    self.layout = Some(Layout::build(index, tree, key));
                }
                let layout = self.layout.as_ref().expect("layout just built");

                let hit = response.hover_pos().and_then(|p| layout.hit(p)).copied();
                // A screenshot is of the view, not of where the pointer rests.
                let hit = if self.shot.is_some() { None } else { hit };
                self.hovered = hit.map(|t| t.node);

                layout.paint(
                    ui.painter(),
                    &treemap::Paint {
                        index,
                        tree,
                        theme: self.theme,
                        lang,
                        metric: self.metric,
                        selected: self.selected,
                        hovered: self.hovered,
                    },
                );

                if let Some(tile) = hit {
                    response.clone().on_hover_ui_at_pointer(|ui| {
                        tile_tooltip(ui, lang, scan, &tile, self.metric)
                    });
                }
                if response.clicked() {
                    if let Some(tile) = hit {
                        actions.push(Action::Select {
                            node: tile.node,
                            from_map: true,
                        });
                    }
                }
                if response.double_clicked() {
                    if let Some(tile) = hit {
                        match tile.what {
                            What::Dir { .. } | What::Rest { .. } => {
                                actions.push(Action::Focus(tile.node))
                            }
                            What::File(_) => {
                                if let Some(parent) = tree.parent(tile.node) {
                                    actions.push(Action::Focus(parent));
                                }
                            }
                        }
                    }
                }
                if response.secondary_clicked() {
                    self.menu_node = hit.map(|t| t.node);
                }
                if let Some(node) = self.menu_node {
                    response.context_menu(|ui| node_menu(ui, lang, scan, node, actions));
                }

                ui.add_space(6.0);
                legend(ui, lang, self.theme);
                ui.label(
                    egui::RichText::new(strings.map_hint)
                        .small()
                        .color(palette.muted),
                );
            });
    }
}

fn tile_tooltip(ui: &mut egui::Ui, lang: Lang, scan: &Scan, tile: &treemap::Tile, metric: Metric) {
    let strings = lang.strings();
    let tree = &scan.tree;
    let index = &scan.index;
    let totals = tree.totals(tile.node);
    ui.set_max_width(460.0);
    match tile.what {
        What::Rest { count, bytes } => {
            ui.label(egui::RichText::new(lang.small_items(count as usize)).strong());
            ui.label(format::size(lang, bytes));
            ui.label(egui::RichText::new(tree.path(index, tile.node)).weak());
        }
        _ => {
            ui.label(egui::RichText::new(tree.name(index, tile.node)).strong());
            ui.label(egui::RichText::new(tree.path(index, tile.node)).weak());
            ui.add_space(4.0);
            let on_disk = format::size(lang, totals.allocated);
            let size = format::size(lang, totals.size);
            let (first, second) = match metric {
                Metric::OnDisk => (
                    (strings.metric_on_disk, on_disk),
                    (strings.metric_size, size),
                ),
                Metric::Size => (
                    (strings.metric_size, size),
                    (strings.metric_on_disk, on_disk),
                ),
            };
            ui.label(format!(
                "{}: {}   ·   {}: {}",
                first.0, first.1, second.0, second.1
            ));
            if let What::File(kind) = tile.what {
                ui.label(format!("{}: {}", strings.col_kind, lang.kind(kind)));
                let entry = &index.entries()[tile.node as usize];
                if entry.is_hardlink() {
                    ui.label(egui::RichText::new(strings.hardlink_note).weak());
                }
                if entry.is_cloud() {
                    ui.label(egui::RichText::new(strings.cloud_note).weak());
                }
            } else {
                ui.label(lang.files(totals.files as u64));
            }
            let date = format::date(totals.newest);
            if !date.is_empty() {
                ui.label(format!("{}: {date}", strings.col_modified));
            }
        }
    }
}

/// Fixed order, every kind that has a colour, then the shared neutral: the
/// legend never reorders or drops an entry as the view changes.
fn legend(ui: &mut egui::Ui, lang: Lang, theme_: Theme) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for kind in theme::COLOURED {
            colour_chip(ui, theme::kind_colour(theme_, kind));
            ui.label(egui::RichText::new(lang.kind(kind)).small());
            ui.add_space(8.0);
        }
        colour_chip(ui, theme::kind_colour(theme_, Kind::Other));
        ui.label(egui::RichText::new(lang.strings().legend_other).small());
    });
}

/* -------------------------------------------------------------------- *
 * Small shared pieces
 * -------------------------------------------------------------------- */

fn value(tree: &burrow_tree::Tree, node: NodeId, metric: Metric) -> u64 {
    let t = tree.totals(node);
    match metric {
        Metric::OnDisk => t.allocated,
        Metric::Size => t.size,
    }
}

fn node_menu(ui: &mut egui::Ui, lang: Lang, scan: &Scan, node: NodeId, actions: &mut Vec<Action>) {
    let strings = lang.strings();
    let path = scan.tree.path(&scan.index, node);
    ui.set_min_width(200.0);
    if ui.button(strings.menu_open).clicked() {
        actions.push(Action::Open(path.clone()));
        ui.close();
    }
    if node != scan.tree.root() && ui.button(strings.menu_reveal).clicked() {
        actions.push(Action::Reveal(path.clone()));
        ui.close();
    }
    if ui.button(strings.menu_copy).clicked() {
        actions.push(Action::Copy(path.clone()));
        ui.close();
    }
    if scan.tree.is_dir(node) && ui.button(strings.menu_zoom).clicked() {
        actions.push(Action::Focus(node));
        ui.close();
    }
    ui.separator();
    let words = lang.words();
    let protected = cleanup::is_protected(&scan.index, &scan.tree, node);
    let button = ui.add_enabled(!protected, egui::Button::new(words.menu_recycle));
    let button = button.on_disabled_hover_text(words.protected);
    if button.clicked() {
        actions.push(Action::Recycle {
            paths: vec![path],
            bytes: scan.tree.totals(node).allocated,
        });
        ui.close();
    }
}

fn heading(ui: &mut egui::Ui, label: &str, right: bool, palette: &theme::Palette) {
    let text = egui::RichText::new(label).small().color(palette.muted);
    if right {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(text);
        });
    } else {
        ui.label(text);
    }
}

fn right_label(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).color(colour)).selectable(false));
    });
}

/// A thin bar of a part against its whole, with the percentage beside it.
fn share_bar(ui: &mut egui::Ui, lang: Lang, part: u64, whole: u64, palette: &theme::Palette) {
    let width = (ui.available_width() - 44.0).clamp(12.0, 80.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 6.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, palette.panel_2);
    let share = (part as f32 / whole.max(1) as f32).clamp(0.0, 1.0);
    if share > 0.0 {
        let filled = egui::Rect::from_min_size(
            rect.min,
            egui::vec2((rect.width() * share).max(2.0), rect.height()),
        );
        painter.rect_filled(filled, 3.0, palette.accent);
    }
    ui.add(
        egui::Label::new(
            egui::RichText::new(format::percent(lang, part, whole))
                .small()
                .color(palette.muted),
        )
        .selectable(false),
    );
}

/// A small padlock, drawn: the emoji is not in every font Windows has, and
/// a missing glyph looks like a stray character.
fn padlock(ui: &mut egui::Ui, colour: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    let body =
        egui::Rect::from_center_size(rect.center() + egui::vec2(0.0, 2.5), egui::vec2(9.0, 7.0));
    painter.rect_filled(body, 1.5, colour);
    let shackle = egui::Rect::from_center_size(
        body.center_top() + egui::vec2(0.0, -2.0),
        egui::vec2(6.0, 7.0),
    );
    painter.rect_stroke(
        shackle,
        3.0,
        egui::Stroke::new(1.5, colour),
        egui::StrokeKind::Middle,
    );
    response
}

fn colour_chip(ui: &mut egui::Ui, colour: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, colour);
}

/// A folder mark, or the colour of the file's kind — the same colour its
/// tile has on the map.
fn swatch(
    ui: &mut egui::Ui,
    theme_: Theme,
    is_dir: bool,
    index: &ferret_core::Index,
    node: NodeId,
    root: NodeId,
) {
    let palette = theme::palette(theme_);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    if is_dir || node == root {
        let body = egui::Rect::from_min_max(
            rect.min + egui::vec2(1.0, 4.0),
            rect.max - egui::vec2(1.0, 2.0),
        );
        let tab = egui::Rect::from_min_size(rect.min + egui::vec2(1.0, 2.0), egui::vec2(5.0, 3.0));
        painter.rect_filled(tab, 1.0, palette.accent);
        painter.rect_filled(body, 2.0, palette.accent);
    } else {
        let kind = Kind::of(index.name(node as usize));
        painter.rect_filled(rect.shrink(2.0), 2.0, theme::kind_colour(theme_, kind));
    }
}

fn waiting(ui: &mut egui::Ui, title: &str, detail: &str, percent: Option<u8>) {
    centred(ui, |ui| {
        ui.add(egui::Spinner::new().size(28.0));
        ui.add_space(14.0);
        ui.label(egui::RichText::new(title).heading());
        if !detail.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(detail).weak());
        }
        if let Some(percent) = percent {
            ui.add_space(14.0);
            ui.add_sized(
                egui::vec2(280.0, 8.0),
                egui::ProgressBar::new(percent as f32 / 100.0).corner_radius(4),
            );
        }
    });
}

/// A message with an optional button. Returns whether it was pressed.
fn card(ui: &mut egui::Ui, title: &str, detail: &str, action: Option<&str>) -> bool {
    let mut clicked = false;
    centred(ui, |ui| {
        ui.label(egui::RichText::new(title).heading());
        if !detail.is_empty() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(detail).weak());
        }
        if let Some(action) = action {
            ui.add_space(16.0);
            clicked = ui.button(action).clicked();
        }
    });
    clicked
}

fn centred(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let height = ui.available_height();
    ui.vertical_centered(|ui| {
        ui.add_space(height * 0.28);
        ui.set_max_width(460.0);
        contents(ui);
    });
}
