// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    // Keep argument parsing inside the updater crate so the helper binary stays
    // a tiny process boundary around the staged replacement engine.
    let result = oxideterm_update::parse_windows_update_helper_options(std::env::args_os())
        .and_then(oxideterm_update::run_windows_update_helper);
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("oxideterm-update-helper is only used on Windows.");
}
