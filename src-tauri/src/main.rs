// Deprecated legacy Tauri shell entrypoint. The default Anchor product is now
// the CLI + Web Admin; this binary remains only for explicit compatibility builds.
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    anchor_lib::run()
}
