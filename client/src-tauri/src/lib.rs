mod commands;

use commands::AppState;
use std::sync::Mutex;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(AppState {
            game: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            commands::initialize_game,
            commands::submit_action,
            commands::get_game_state,
            commands::get_legal_actions,
            commands::get_ai_action,
            commands::dispose_game,
        ])
        .run(tauri::generate_context!())
        .expect("error while running phase.rs");
}
