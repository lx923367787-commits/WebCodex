mod activity;
mod ai_office_commands;
mod bundled_tools;
mod cli_catalog;
mod commands;
mod deadline;
mod desktop_shell;
mod error;
mod host_control_config;
mod models;
mod operation;
mod platform;
mod process;
mod state;
mod tray;
mod tunnel_config;
mod webcodex;

use state::AppState;
use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

/// Local demo transport shares the desktop policy validator and CLI discovery.
/// The preview gateway supplies its own isolated data directory.
pub async fn ai_office_preview_request(
    data_dir: &std::path::Path,
    request: serde_json::Value,
) -> serde_json::Value {
    use serde_json::json;
    if request["op"] == "discover_cli" {
        return match cli_catalog::discover_ai_office_clis() {
            Ok(installed) => json!({"success":true,"output":installed}),
            Err(error) => json!({"success":false,"error":error}),
        };
    }
    if request["op"] == "refresh_cli" {
        return match cli_catalog::refresh_ai_office_cli(
            request["providerId"].as_str().unwrap_or_default().into(),
        )
        .await
        {
            Ok(catalog) => json!({"success": true, "output": catalog}),
            Err(error) => json!({"success": false, "error": error}),
        };
    }
    match state::ai_office::preview_request(data_dir, request) {
        Ok(output) => json!({"success": true, "output": output}),
        Err(error) => json!({"success": false, "error": error}),
    }
}

fn desktop_context() -> tauri::Context<tauri::Wry> {
    // Keep one macro expansion: macOS development builds embed a fixed-name
    // Info.plist symbol, shared by the asset probe and normal startup.
    tauri::generate_context!()
}

/// Packaging-only probe: no window, plugins, credentials or runtime processes.
/// A Cargo build without tauri/custom-protocol can compile successfully while
/// still pointing at the development server, which cannot resume a saved runtime.
pub fn has_embedded_frontend() -> bool {
    if tauri::is_dev() {
        return false;
    }
    let context = desktop_context();
    let names: Vec<_> = context
        .assets()
        .iter()
        .map(|(name, _)| name.into_owned())
        .collect();
    names
        .iter()
        .any(|name| name.trim_start_matches('/') == "index.html")
        && names.iter().any(|name| name.ends_with(".js"))
        && names.iter().any(|name| name.ends_with(".css"))
}

pub fn run() {
    let app = tauri::Builder::default()
        // Tauri recommends registering single-instance first so a secondary
        // process is rejected before any other plugin can initialize state.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            desktop_shell::handle_second_instance(app, &argv);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .setup(|app| {
            let data_dir = app.path().app_local_data_dir()?;
            let startup_log = data_dir.join("runtime-startup.log");
            let resource_dir = app.path().resource_dir()?;
            app.manage(AppState::new(data_dir, resource_dir)?);
            let host_observer = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                host_observer
                    .state::<AppState>()
                    .observe_regular_tunnel_host()
                    .await;
            });
            let control_publisher = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                control_publisher
                    .state::<AppState>()
                    .publish_host_control()
                    .await;
            });
            app.manage(desktop_shell::DesktopShellState::default());
            app.manage(tray::TrayPresentationCache::default());
            tray::setup(app.handle())?;

            let snapshot = app.state::<AppState>().get_state();
            tray::refresh_from_snapshot(app.handle(), &snapshot);
            if !desktop_shell::is_background_launch(std::env::args()) {
                desktop_shell::show_main_window(app.handle())?;
            }
            // Saved runtime recovery must also work before a hidden WebView loads.
            if snapshot.runtime_autostart
                && snapshot
                    .topology
                    .as_ref()
                    .is_some_and(|topology| topology.experience == models::Experience::Full)
            {
                let startup = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let state = startup.state::<AppState>();
                    let _ = std::fs::write(&startup_log, "resuming_runtime\n");
                    let result = async {
                        let next = state.resume_saved_runtime().await?;
                        if next.preferred_connection
                            == models::RegularConnectionPreference::OpenAiTunnel
                            && next.topology.as_ref().is_some_and(|topology| {
                                topology.experience == models::Experience::Full
                                    && matches!(topology.server, models::ServerTopology::Local)
                            })
                            && next.readiness.services_ready()
                            && next.openai_tunnel_configured
                            && next.regular_tunnel.is_none()
                        {
                            let _ = std::fs::write(&startup_log, "starting_tunnel\n");
                            state.start_regular_tunnel().await
                        } else {
                            Ok(next)
                        }
                    }
                    .await;
                    let status = match &result {
                        Ok(_) => "ready\n".to_owned(),
                        Err(error) => format!("error: {}\n", error.code),
                    };
                    if let Err(error) = std::fs::write(&startup_log, status) {
                        eprintln!("WebCodex startup log failed: {error}");
                    }
                    tray::refresh_from_snapshot(&startup, &state.get_state());
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ai_office_commands::get_ai_office_snapshot,
            ai_office_commands::save_ai_office_policy,
            ai_office_commands::evaluate_ai_office_plan,
            ai_office_commands::ai_office_team_call,
            cli_catalog::refresh_ai_office_cli,
            cli_catalog::discover_ai_office_clis,
            commands::get_desktop_state,
            commands::open_powershell_install_guide,
            commands::get_launch_at_login,
            commands::set_launch_at_login,
            commands::refresh_runtime_status,
            commands::observe_chatgpt_activity,
            commands::resume_saved_runtime,
            commands::update_tunnel_proxy,
            commands::update_tunnel_config,
            commands::inspect_project,
            commands::configure_local_setup,
            commands::activate_local_project,
            commands::list_named_workspaces,
            commands::save_named_workspaces,
            commands::list_desktop_projects,
            commands::add_desktop_project,
            commands::switch_desktop_project,
            commands::mutate_desktop_project,
            commands::configure_remote_setup,
            commands::start_quick_share,
            commands::stop_quick_share,
            commands::start_regular_tunnel,
            commands::stop_regular_tunnel,
            commands::stop_local_runtime,
            commands::cancel_desktop_operation,
            commands::get_bounded_activity,
        ])
        .build(desktop_context())
        .expect("failed to build WebCodex Desktop");

    app.run(|app_handle, event| match event {
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == desktop_shell::MAIN_WINDOW_LABEL => {
            let shell = app_handle.state::<desktop_shell::DesktopShellState>();
            if shell.close_disposition() == desktop_shell::CloseDisposition::HideWindow {
                api.prevent_close();
                let _ = desktop_shell::hide_main_window(app_handle);
            }
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen {
            has_visible_windows,
            ..
        } => {
            if !has_visible_windows {
                let _ = desktop_shell::show_main_window(app_handle);
            }
        }
        tauri::RunEvent::ExitRequested { .. } => {
            app_handle
                .state::<desktop_shell::DesktopShellState>()
                .mark_exit_requested();
            let state = app_handle.state::<AppState>();
            tauri::async_runtime::block_on(state.shutdown());
        }
        tauri::RunEvent::Exit => {
            let state = app_handle.state::<AppState>();
            tauri::async_runtime::block_on(state.shutdown());
        }
        _ => {}
    });
}
