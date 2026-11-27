mod commands;
mod netdebug;
mod p2p;
mod proxy;
mod security;
mod server;
mod state;
mod storage;

use tauri::Manager;

use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "popstall_lib=info,iroh=info".into()),
        )
        .try_init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            crate::netdebug::init(&dir);
            app.manage(AppState::new(dir));
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.maximize();
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                if let Err(err) = state.node().await {
                    tracing::warn!(%err, "P2P node failed to start");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan_folder,
            commands::start_host,
            commands::stop_host,
            commands::host_status,
            commands::open_preview,
            commands::open_guest,
            commands::list_plaza,
            commands::talk_snapshot,
            commands::talk_send,
            commands::traffic,
            commands::route_status,
            commands::helper_status,
            commands::helper_set,
            commands::helper_list,
            commands::use_helper,
            commands::open_discovered,
            commands::place_stage_bounds,
            commands::close_stage
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
