mod announcer;
mod allowlist;
mod archive;
mod associations;
mod changelog;
mod commands;
mod credentials;
mod curseforge;
mod decompile;
mod defender;
mod discord;
mod discord_commands;
mod export;
mod file_commands;
mod files;
mod git;
mod git_commands;
mod git_install;
mod global_packs;
mod java;
mod history;
mod gitops;
mod http;
mod instance;
mod instance_locks;
mod jobs;
mod keychain;
mod join;
mod launch;
mod links;
mod minecraft_auth;
mod minecraft_commands;
mod logs;
mod mirror;
mod mod_commands;
mod mod_list;
mod mod_manifest;
mod mod_updates;
mod modrinth;
mod modsync;
mod nbt;
mod os_drag;
mod pack;
mod pack_commands;
mod process;
mod publish;
mod remote;
mod self_update;
mod server_icon;
mod settings;
mod setup;
mod sides;
mod system_commands;
mod testrun;
mod twins;
mod text_search;
mod watcher;
#[cfg(test)]
mod live_tests;

use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use commands::AppState;
use settings::Settings;

const STARTED_HIDDEN_ARG: &str = "--hidden";
pub(crate) const TRAY_ID: &str = "main";

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Runs a command from the pack's git hooks and returns its exit code, or `None` to start the app.
pub fn run_hook_command() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if !matches!(args.first(), Some(&(setup::MODSYNC_COMMAND | setup::IRIS_CLEAN_COMMAND))) {
        return None;
    }
    let dir = std::env::current_dir().unwrap_or_default();
    let log = jobs::Log::terminal();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(async {
        match args[..] {
            [setup::IRIS_CLEAN_COMMAND] => iris_clean_stdio(),
            [_, "lock"] => modsync::lock(&dir, &log).await,
            [_, "adopt"] => modsync::adopt(&dir, &log).await.map(|_| ()),
            [_, "restore"] => modsync::restore(&dir, &log).await,
            [_, "status"] => modsync::status(&dir, &log),
            _ => Err(anyhow::anyhow!("usage: packraven {} lock|adopt|restore|status", setup::MODSYNC_COMMAND)),
        }
    });
    match result {
        Ok(()) => Some(0),
        Err(e) => {
            eprintln!("modsync: {e:#}");
            Some(1)
        }
    }
}

fn iris_clean_stdio() -> anyhow::Result<()> {
    use std::io::{Read, Write};
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    std::io::stdout().write_all(modsync::iris_clean(&String::from_utf8_lossy(&input)).as_bytes())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![STARTED_HIDDEN_ARG])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let settings_path = config_dir.join("settings.json");
            let _ = java::DOWNLOAD_DIR.set(app.path().app_data_dir()?.join("java"));
            let settings = Settings::load(&settings_path);
            settings.request_folder_access();
            app.manage(AppState {
                settings: Mutex::new(settings),
                settings_path,
                data_dir: app.path().app_data_dir()?,
                jobs: jobs::Jobs::default(),
                mod_changes: Arc::default(),
                test: Arc::new(testrun::TestRun::default()),
                watcher: Mutex::new(None),
                watch_error: Mutex::default(),
                history: Mutex::default(),
                mod_list: Mutex::default(),
                minecraft: Arc::default(),
                minecraft_sign_in: Mutex::default(),
            });
            app.state::<AppState>().rewatch(app.handle());
            announcer::start(app.handle().clone());
            app.manage(self_update::SelfUpdate::default());
            self_update::start(app.handle().clone());

            let show = MenuItem::with_id(app, "show", "Show packraven", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::with_id(TRAY_ID)
                .icon(app.default_window_icon().cloned().expect("bundled icon"))
                .tooltip("packraven")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            if !std::env::args().any(|a| a == STARTED_HIDDEN_ARG) {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::overview,
            commands::get_pack_config,
            commands::save_pack_config,
            commands::set_version,
            commands::version_problem,
            commands::get_notes,
            commands::save_notes,
            commands::changelog_preview,
            commands::set_compare_baseline,
            commands::clear_compare_baseline,
            commands::resync_mod_list,
            commands::track_directly,
            commands::revert_mods,
            git_commands::git_pull_preview,
            commands::server_sides,
            commands::set_side_override,
            mod_commands::installed_mods,
            mod_commands::check_mod_updates,
            mod_commands::mod_versions,
            mod_commands::set_update_channel,
            mod_commands::search_mods,
            mod_commands::mod_categories,
            mod_commands::modrinth_counterparts,
            mod_commands::curseforge_counterparts,
            mod_commands::preview_mods,
            mod_commands::change_mods,
            mod_commands::cancel_mod_change,
            mod_commands::set_up_version_checker,
            mod_commands::set_mod_locked,
            mod_commands::set_mods_enabled,
            mod_commands::mod_changelog,
            mod_commands::mod_description,
            mod_commands::remove_mods,
            commands::publish_preflight,
            commands::build_client_export,
            commands::build_modrinth_export,
            commands::upload_release_to_modrinth,
            commands::upload_release_server_pack,
            commands::finish_release,
            commands::mark_server_pack_uploaded,
            commands::replace_curseforge_file,
            commands::release_attention,
            commands::set_override_choice,
            commands::modrinth_project_title,
            commands::look_up_override_candidates,
            commands::override_choices,
            commands::set_modrinth_author,
            commands::use_modrinth_build,
            commands::published_to,
            commands::unfinished_releases,
            commands::build_server_pack,
            commands::start_test,
            commands::start_client,
            commands::server_command,
            commands::kill_process,
            commands::publish,
            commands::releases,
            commands::output_dir,
            pack_commands::list_packs,
            pack_commands::discover_instances,
            pack_commands::inspect_instance,
            pack_commands::setup_instance,
            pack_commands::join_pack,
            pack_commands::switch_pack,
            pack_commands::remove_pack,
            pack_commands::update_hooks,
            pack_commands::get_pack_java,
            pack_commands::set_pack_java,
            pack_commands::get_run_args,
            pack_commands::set_run_args,
            pack_commands::get_server_icon,
            pack_commands::set_server_icon,
            pack_commands::use_project_logo_as_server_icon,
            pack_commands::remove_server_icon,
            pack_commands::server_launch,
            git_commands::git_overview,
            git_commands::git_status,
            git_commands::git_diff,
            git_commands::git_blob,
            git_commands::git_apply_lines,
            git_commands::git_stage,
            git_commands::git_unstage,
            git_commands::git_discard,
            git_commands::git_ignore,
            git_commands::git_ignore_options,
            git_commands::git_ignore_preview,
            git_commands::git_untrack_preview,
            git_commands::git_keep_tracking,
            git_commands::git_untrack,
            git_commands::git_commit,
            git_commands::git_sync,
            file_commands::list_dir,
            file_commands::read_file,
            file_commands::write_file,
            file_commands::absolute_path,
            file_commands::create_file,
            file_commands::create_dir,
            file_commands::trash_paths,
            file_commands::copy_files,
            file_commands::paste_files,
            file_commands::rename_path,
            file_commands::rename_paths,
            file_commands::move_paths,
            file_commands::open_pack_folder,
            file_commands::index_files,
            file_commands::search_text,
            file_commands::replace_text,
            file_commands::copy_paths,
            file_commands::open_with_default_app,
            file_commands::default_app,
            file_commands::read_bytes,
            file_commands::read_nbt,
            file_commands::read_log,
            file_commands::decompile_class,
            file_commands::extract_archive,
            file_commands::compress_paths,
            file_commands::media_path,
            file_commands::pack_loaders,
            file_commands::pack_folder,
            file_commands::copy_into_pack,
            file_commands::import_paths,
            file_commands::drag_out,
            file_commands::file_history,
            file_commands::file_undo,
            system_commands::defender_status,
            system_commands::defender_exclude,
            system_commands::defender_dismiss,
            system_commands::accept_minecraft_eula,
            minecraft_commands::minecraft_account,
            minecraft_commands::minecraft_sign_in,
            minecraft_commands::minecraft_sign_in_cancel,
            minecraft_commands::minecraft_sign_out,
            system_commands::git_version,
            system_commands::install_git,
            system_commands::scan_java,
            system_commands::download_java,
            system_commands::cancel_java_download,
            system_commands::pack_java_default,
            system_commands::credentials_status,
            system_commands::credential_store,
            system_commands::credential_use_env,
            system_commands::credential_clear,
            system_commands::github_identity,
            system_commands::env_var_names,
            system_commands::set_icon_theme,
            system_commands::update_status,
            system_commands::check_for_update,
            system_commands::install_update,
            system_commands::set_auto_update,
            system_commands::update_changelog,
            system_commands::dismiss_update_changelog,
            discord_commands::get_webhook,
            discord_commands::set_webhook,
            discord_commands::discord_test,
            discord_commands::announce_release,
            discord_commands::discohook_link,
            discord_commands::discohook_preview,
        ])
        .build(tauri::generate_context!())
        .expect("error while building packraven")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                self_update::install_on_exit(app);
            }
        });
}
