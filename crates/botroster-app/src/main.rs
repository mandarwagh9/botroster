//! BOTROSTER, the desktop client for botroster (SPEC §9).
//!
//! All logic lives in the library crate so the command layer can be driven
//! by tests on `tauri::test`'s mock runtime; a binary crate cannot be
//! imported by one.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    botroster_app::run();
}
