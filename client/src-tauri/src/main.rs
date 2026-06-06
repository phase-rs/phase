#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// 启动Phase Tauri应用程序。
fn main() {
    phase_tauri::run();
}
