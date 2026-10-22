mod audio;
mod cleanup;
mod coding;
mod command;
mod commands;
mod device;
mod dictation;
mod engine;
mod gain;
mod helper;
mod history;
mod learn;
mod live;
mod media;
mod meeting;
mod mic_test;
mod models;
mod paste;
mod pill;
mod pipeline;
mod platform;
mod playback;
mod settings;
mod snippets;
mod spoken;
mod stream;
mod sync;
mod text;
mod tone;
mod tray;
mod update;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, TryLockError};

use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

use crate::{
    cleanup::Cleaner,
    device::DeviceInfo,
    dictation::{Controller, DictationState, Event},
    engine::Engine,
    history::History,
    models::{ModelInfo, ModelStore},
    settings::{Settings, SettingsStore},
};

/// Benchmarks for development (`cargo run --release --example transcribe_wav`,
/// or `clean_text` for transcript cleanup).
#[doc(hidden)]
pub mod devtools {
    use std::{path::Path, time::Instant};

    pub use crate::settings::{CleanupBackend, CleanupSettings};

    /// Downloads a catalog model into `models_dir`, printing progress.
    pub fn download(
        model_id: &str,
        models_dir: &Path,
        accelerator: Option<bool>,
    ) -> Result<(), String> {
        let store = crate::models::ModelStore::new(models_dir.to_path_buf());
        tauri::async_runtime::block_on(store.download(model_id, accelerator, |p| {
            if p.total > 0 {
                println!(
                    "  {}: {:.0}%",
                    p.id,
                    p.downloaded as f64 / p.total as f64 * 100.0
                );
            }
        }))?;
        let status = store
            .statuses()
            .into_iter()
            .chain(store.cleanup_statuses())
            .find(|s| s.info.id == model_id)
            .ok_or("Unknown model id")?;
        println!(
            "downloaded={} accelerator={:?}",
            status.downloaded, status.accelerator
        );
        Ok(())
    }

    /// Cleans up each of `texts` with a built-in model from `models_dir`, or
    /// with a local server, printing the model's reply, what would be pasted,
    /// and how long it took. The first run includes loading the model. A text
    /// that fails is reported and skipped.
    pub fn cleanup(
        settings: &CleanupSettings,
        models_dir: &Path,
        texts: &[String],
        runs: usize,
    ) -> Result<(), String> {
        let store = crate::models::ModelStore::new(models_dir.to_path_buf());
        let cleaner = crate::cleanup::Cleaner::default();
        for text in texts {
            println!("text: {text}");
            for run in 1..=runs {
                let started = Instant::now();
                let reply = match cleaner.reply(settings, &store, text) {
                    Ok(reply) => reply,
                    Err(e) => {
                        println!("  run {run}: failed: {e}");
                        break;
                    }
                };
                println!("  run {run}: {:.2?}", started.elapsed());
                if run == runs {
                    println!("  reply: {reply}");
                    match crate::cleanup::accept(text, &reply) {
                        Some(cleaned) => println!("  pasted: {cleaned}"),
                        None => println!("  pasted: {text} (the reply was rejected)"),
                    }
                }
            }
        }
        Ok(())
    }

    /// Loads a catalog model from `models_dir` and transcribes each audio file
    /// once to warm up, then `runs` more times, printing the load time and the
    /// fastest and median transcription times.
    pub fn benchmark(
        model_id: &str,
        models_dir: &Path,
        files: &[&Path],
        language: &str,
        runs: usize,
    ) -> Result<(), String> {
        let model = crate::models::find(model_id).ok_or("Unknown model id")?;
        let store = crate::models::ModelStore::new(models_dir.to_path_buf());

        crate::engine::silence_logs();
        let mut engine = crate::engine::Engine::default();
        let started = Instant::now();
        engine.load(model, &store.path(model))?;
        println!("{model_id}: loaded in {:.2?}", started.elapsed());

        for file in files {
            let (samples, duration) = crate::media::decode_file(file)?;
            let text = engine.transcribe(&samples, language, "")?;
            let mut times: Vec<_> = (0..runs.max(1))
                .map(|_| {
                    let started = Instant::now();
                    engine
                        .transcribe(&samples, language, "")
                        .map(|_| started.elapsed())
                })
                .collect::<Result<_, _>>()?;
            times.sort();
            let median = times[times.len() / 2];
            println!(
                "  {duration:>5.1}s of audio: median {median:>8.2?}, fastest {:>8.2?} ({:.0}x real time)",
                times[0],
                duration / median.as_secs_f64()
            );
            println!("    text: {}", text.trim());
        }
        Ok(())
    }
}

/// Locking that ignores poisoning. The data behind these locks stays usable
/// after a panic interrupted an update, and one failed dictation shouldn't make
/// every later command panic too.
pub(crate) trait LockExt<T> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T>;
    /// `None` only when another thread holds the lock.
    fn try_lock_unpoisoned(&self) -> Option<MutexGuard<'_, T>>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_unpoisoned(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn try_lock_unpoisoned(&self) -> Option<MutexGuard<'_, T>> {
        match self.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

pub(crate) struct AppState {
    settings: RwLock<Settings>,
    settings_store: SettingsStore,
    pub models: ModelStore,
    /// Held for the whole of a model load or transcription.
    pub engine: Mutex<Engine>,
    /// The model currently being loaded into memory, if any.
    pub engine_loading: Mutex<Option<String>>,
    /// Optional transcript cleanup by a local language model.
    pub cleaner: Arc<Cleaner>,
    pub history: Mutex<History>,
    pub controller: Controller,
    pub dictation_state: Mutex<DictationState>,
    /// Set when the saved hotkey couldn't be registered at launch.
    pub hotkey_error: Mutex<Option<String>>,
    device: OnceLock<DeviceInfo>,
}

impl AppState {
    pub fn settings(&self) -> Settings {
        self.settings
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn replace_settings(&self, settings: Settings) -> Result<(), String> {
        self.update_settings(|current| *current = settings)
    }

    /// Changes settings and saves them as one step, so changes made at the same
    /// time from different threads can't overwrite each other. Nothing changes
    /// if saving fails.
    pub fn update_settings<T>(&self, change: impl FnOnce(&mut Settings) -> T) -> Result<T, String> {
        let mut settings = self
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let mut updated = settings.clone();
        let result = change(&mut updated);
        self.settings_store.save(&updated)?;
        *settings = updated;
        Ok(result)
    }

    /// Detected once; reading the CPU and memory takes a moment.
    pub fn device(&self) -> &DeviceInfo {
        self.device.get_or_init(device::detect)
    }

    /// Loads `model` into `engine`, telling the UI while it happens.
    pub fn load_model(
        &self,
        app: &AppHandle,
        engine: &mut Engine,
        model: &ModelInfo,
    ) -> Result<(), String> {
        if engine.loaded_model() == Some(model.id) {
            return Ok(());
        }
        *self.engine_loading.lock_unpoisoned() = Some(model.id.to_string());
        let _ = app.emit("engine-changed", ());
        let result = engine.load(model, &self.models.path(model));
        *self.engine_loading.lock_unpoisoned() = None;
        let _ = app.emit("engine-changed", ());
        result
    }

    /// The speech model to use now: the selected one, if it's downloaded.
    pub fn selected_model(&self) -> Option<&'static ModelInfo> {
        models::find(&self.settings().selected_model).filter(|m| self.models.is_downloaded(m.id))
    }

    /// Loads a downloaded model in the background so the first dictation is fast.
    pub fn warm_up(&self, app: &AppHandle, id: &str) {
        if models::find(id).is_none() || !self.models.is_downloaded(id) {
            return;
        }
        let app = app.clone();
        let spawned = std::thread::Builder::new()
            .name("warm-up".into())
            .spawn(move || {
                let state = app.state::<AppState>();
                let mut engine = state.engine.lock_unpoisoned();
                // Checked once the engine is ours: when models are switched
                // quickly, warm-ups queue up, and loading the ones no longer
                // selected would hold up dictation for nothing.
                let Some(model) = state.selected_model() else {
                    return;
                };
                if let Err(e) = state.load_model(&app, &mut engine, model) {
                    eprintln!("[models] warm-up failed: {e}");
                }
            });
        if let Err(e) = spawned {
            eprintln!("[models] couldn't start warm-up: {e}");
        }
    }
}

/// Passed to WordFlow when the system starts it at login.
const AT_LOGIN_ARG: &str = "--at-login";

/// Turns starting at login on or off with the system.
pub(crate) fn set_launch_at_login(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let autostart = app.autolaunch();
    let result = if enabled {
        autostart.enable()
    } else {
        autostart.disable()
    };
    result.map_err(|e| format!("Couldn't change launch at login: {e}"))
}

/// Keeps the system's login item in step with the setting.
fn sync_launch_at_login(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let registered = app.autolaunch().is_enabled().unwrap_or(false);
    if enabled || registered {
        set_launch_at_login(app, enabled)?;
    }
    Ok(())
}

/// Maximizes the main window when its default size doesn't fit the screen, as
/// on a 1366×768 laptop, where the onboarding footer would otherwise sit below
/// the bottom of the screen. The window isn't placed yet, so its frame size is
/// unknown; maximizing leaves the title bar and panels to the window manager.
fn fit_to_screen(window: &tauri::WebviewWindow) {
    let (Ok(Some(monitor)), Ok(size)) = (window.current_monitor(), window.inner_size()) else {
        return;
    };
    let area = monitor.work_area().size;
    if size.width > area.width || size.height > area.height {
        let _ = window.maximize();
    }
}

/// Builds the main window again after it was closed, from its entry in
/// tauri.conf.json. `route` picks the screen it opens on.
pub(crate) fn create_main_window(
    app: &AppHandle,
    route: Option<&str>,
) -> tauri::Result<tauri::WebviewWindow> {
    let mut config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == "main")
        .cloned()
        .ok_or(tauri::Error::WindowNotFound)?;
    if let Some(route) = route {
        config.url = tauri::WebviewUrl::App(format!("index.html#{route}").into());
    }
    let window = tauri::WebviewWindowBuilder::from_config(app, &config)?.build()?;
    platform::style_main_window(&window);
    fit_to_screen(&window);
    Ok(window)
}

pub fn run() {
    let app = tauri::Builder::default()
        // Launching WordFlow again brings the running app forward instead of
        // starting a second copy with its own tray icon and hotkey.
        .plugin(
            tauri_plugin_single_instance::Builder::new()
                .callback(|app, _args, _cwd| {
                    tray::open_main_window(app, None);
                })
                // The bundle identifier can't be the D-Bus name on Linux: "2048labs"
                // starts with a digit, which D-Bus rejects.
                .dbus_id("com.labs2048.speaktype")
                .build(),
        )
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                // The login item's name from before the rename to WordFlow, so existing
                // login items are updated rather than duplicated.
                .app_name("Wordfall")
                .arg(AT_LOGIN_ARG)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(update::PendingUpdate::default())
        .setup(|app| {
            engine::silence_logs();
            let settings_store = SettingsStore::new(app.path().app_config_dir()?);
            let settings = settings_store.load();
            let data_dir = app.path().app_data_dir()?;
            let history = History::load(&data_dir);
            let show_tray_icon = settings.show_tray_icon;
            // Started at login: stay in the menu bar or tray until needed.
            let in_background = std::env::args().any(|arg| arg == AT_LOGIN_ARG)
                && settings.has_completed_onboarding
                && show_tray_icon;

            app.manage(AppState {
                settings: RwLock::new(settings),
                settings_store,
                models: ModelStore::new(data_dir.join("models")),
                engine: Mutex::default(),
                engine_loading: Mutex::new(None),
                cleaner: Arc::default(),
                history: Mutex::new(history),
                controller: Controller::spawn(app.handle().clone())?,
                dictation_state: Mutex::new(DictationState::Idle),
                hotkey_error: Mutex::new(None),
                device: OnceLock::new(),
            });
            app.manage(meeting::Meetings::new(data_dir.join("meetings")));
            app.manage(mic_test::MicTest::default());

            if let Some(main) = app.get_webview_window("main") {
                platform::style_main_window(&main);
                fit_to_screen(&main);
                if in_background {
                    tray::close_main_window(app.handle(), &main);
                }
            }

            // Shortcut registration waits on the main thread, so it can't run
            // here before the event loop starts.
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("startup".into())
                .spawn(move || {
                    let state = handle.state::<AppState>();
                    // Read now rather than captured above, in case the UI has
                    // already saved a change.
                    let settings = state.settings();
                    let result = state
                        .controller
                        .register_hotkey(&handle, &settings.hotkey, None);
                    if let Err(e) = result {
                        eprintln!("[hotkey] {e}");
                        *state.hotkey_error.lock_unpoisoned() = Some(e);
                    }
                    if let Err(e) = command::register_hotkey(
                        &handle,
                        &state.controller,
                        &settings.command_hotkey,
                        None,
                    ) {
                        eprintln!("[hotkey] command shortcut: {e}");
                    }
                    state.controller.send(Event::RefreshPill);
                    state.warm_up(&handle, &settings.selected_model);
                    state.cleaner.warm_up(&settings.cleanup, &state.models);
                    // Re-registered each launch, so it follows the app if it moves.
                    let at_login = sync_launch_at_login(&handle, settings.launch_at_login);
                    if let Err(e) = at_login {
                        eprintln!("[startup] {e}");
                    }
                    sync::start(&handle);
                    state.device();
                })?;

            tray::build(app.handle(), show_tray_icon)?;
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // Closing the main window keeps WordFlow running in the menu bar or tray.
            ("main", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                if let Some(main) = window.app_handle().get_webview_window("main") {
                    tray::close_main_window(window.app_handle(), &main);
                }
            }
            // The menu bar panel behaves like a popover: it closes when you click elsewhere.
            (tray::PANEL_LABEL, WindowEvent::Focused(false)) => {
                tray::panel_blurred(window.app_handle())
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_settings,
            commands::save_settings,
            commands::list_input_devices,
            commands::get_sync_status,
            commands::pick_sync_folder,
            commands::sync_now,
            commands::export_library,
            commands::import_library,
            commands::list_models,
            commands::get_engine_status,
            commands::get_device_info,
            commands::download_model,
            commands::cancel_download,
            commands::delete_model,
            commands::get_cleanup_status,
            commands::list_server_models,
            commands::test_cleanup_api,
            commands::get_app_tone,
            commands::get_history,
            commands::get_stats,
            commands::delete_history_item,
            commands::edit_history_item,
            commands::forget_words,
            commands::clear_history,
            commands::read_history_audio,
            commands::reveal_history_audio,
            mic_test::start_mic_test,
            mic_test::stop_mic_test,
            mic_test::cancel_mic_test,
            mic_test::transcribe_mic_test,
            commands::toggle_dictation,
            commands::get_dictation_state,
            commands::get_permissions,
            commands::request_permission,
            commands::open_permission_settings,
            update::check_for_update,
            update::install_update,
            commands::open_main_window,
            commands::hide_tray_panel,
            commands::quit_app,
            meeting::commands::get_meeting_setup,
            meeting::commands::get_meeting_state,
            meeting::commands::start_meeting,
            meeting::commands::stop_meeting,
            meeting::commands::list_meetings,
            meeting::commands::get_meeting,
            meeting::commands::rename_meeting,
            meeting::commands::delete_meeting,
            meeting::commands::summarize_meeting,
            meeting::commands::reveal_meeting_audio,
            meeting::commands::export_meeting_markdown,
        ])
        .build(tauri::generate_context!())
        .expect("error while building WordFlow");

    app.run(|app, event| {
        #[cfg(not(target_os = "macos"))]
        let _ = app;
        match event {
            // Closing the last window doesn't quit; only Quit does.
            tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } => api.prevent_exit(),
            // macOS: clicking the Dock icon or opening the app again brings the window back.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => tray::open_main_window(app, None),
            _ => {}
        }
    });
}
