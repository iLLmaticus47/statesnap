//! StateSnap: one hotkey copies a game's save folder into a named snapshot,
//! one click puts a snapshot back. Sits in the tray.

#![windows_subsystem = "windows"]

mod config;
mod lang;
mod snapshot;
mod steam;
mod ui;
mod winutil;

use native_windows_gui as nwg;

fn main() {
    if !winutil::claim_single_instance() {
        // Already running: ask that instance to show its window instead.
        if !winutil::poke_running_instance(ui::MAIN_TITLE, ui::WM_SHOW_MANAGER) {
            let s = lang::pick("auto", winutil::system_language_is_serbian());
            nwg::simple_message(ui::MAIN_TITLE, s.already_running);
        }
        return;
    }

    // No console in a GUI app: leave a trace if something goes badly wrong.
    std::panic::set_hook(Box::new(|info| {
        winutil::resume_suspended(); // never leave a game frozen
        let dir = config::app_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("crash.log"), format!("{}\n", info));
    }));

    if let Err(e) = ui::run() {
        nwg::error_message(ui::MAIN_TITLE, &format!("{}", e));
    }
}
