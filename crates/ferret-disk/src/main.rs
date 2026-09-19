//! Ferret Disk — why is my disk full?
//!
//! Reads an NTFS volume's master file table in seconds, adds up every folder,
//! and draws the result as a treemap next to a folder tree. The engine is
//! Ferret's; everything here is the window.

// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod format;
mod i18n;
mod prefs;
mod rows;
mod shell;
mod theme;
mod treemap;
mod worker;

use eframe::egui;

const INITIAL_SIZE: [f32; 2] = [1280.0, 800.0];
const MINIMUM_SIZE: [f32; 2] = [860.0, 520.0];

fn main() -> eframe::Result {
    // `--screenshot out.png`: scan, save a picture of the window, exit. For
    // documentation, and for checking the window on a machine where it runs
    // elevated and cannot be driven from outside.
    let args: Vec<String> = std::env::args().collect();
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Ferret Disk")
        .with_inner_size(INITIAL_SIZE)
        .with_min_inner_size(MINIMUM_SIZE)
        .with_clamp_size_to_monitor_size(true)
        .with_app_id("dev.ferret.disk");

    if let Some(icon) = icon() {
        viewport = viewport.with_icon(icon);
    }

    eframe::run_native(
        "Ferret Disk",
        eframe::NativeOptions {
            viewport,
            centered: true,
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(app::DiskApp::new(cc, screenshot)))),
    )
}

/// The taskbar and title-bar icon. Missing is not fatal: Windows has a default.
fn icon() -> Option<egui::IconData> {
    const PNG: &[u8] = include_bytes!("../../../icons/256x256.png");
    let decoded = image::load_from_memory(PNG).ok()?.into_rgba8();
    let (width, height) = decoded.dimensions();
    Some(egui::IconData {
        rgba: decoded.into_raw(),
        width,
        height,
    })
}
