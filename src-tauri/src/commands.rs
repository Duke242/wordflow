//! Commands the UI calls through `invoke`.
//!
//! All are async so they run off the main thread: shortcut registration waits
//! on the main thread, and on Windows the audio APIs can't be used from it.
//! Work that can block for a while, like waiting for the engine, runs on the
//! blocking thread pool so it doesn't hold up other commands.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, ipc::Response};
use tauri_plugin_opener::OpenerExt;

use crate::{
    AppState, LockExt,
    audio::{self, InputDevice},
    cleanup, command,
    device::{self, DeviceInfo, Recommendation},
    dictation::{DictationState, Event},
    history::{HistoryItem, StatsEntry},
    learn,
    models::{self, ModelStatus},
    platform::{self, Permission, PermissionKind},
    settings::{CleanupBackend, CleanupSettings, Settings},
    sync,
    text::DictionaryEntry,
    tone::{self, AppCategory},
    tray,
};

type CommandResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    os: &'static str,
    version: String,
    /// The git tag and commit this was built from, e.g. "v2.0.0-alpha.8".
    build: &'static str,
    setup_notes: Vec<String>,
    hotkey_error: Option<String>,
    /// The hotkey is a single modifier key, but the OS won't let us listen for it.
    hotkey_blocked: bool,
    /// Single-modifier hotkeys this OS supports, e.g. "Fn".
    modifier_hotkeys: &'static [&'static str],
}

#[tauri::command]
pub async fn get_status(app: AppHandle, state: State<'_, AppState>) -> CommandResult<Status> {
    Ok(Status {
        os: std::env::consts::OS,
        version: app.package_info().version.to_string(),
        build: env!("WORDFLOW_BUILD"),
        setup_notes: platform::setup_notes(),
        hotkey_error: state.hotkey_error.lock_unpoisoned().clone(),
        hotkey_blocked: platform::modifier_hotkey_blocked(),
        modifier_hotkeys: platform::MODIFIER_HOTKEYS,
    })
}

// ---- Settings ----

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> CommandResult<Settings> {
    Ok(state.settings())
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    mut settings: Settings,
    changed: Option<Vec<String>>,
) -> CommandResult<Settings> {
    let previous = state.settings();
    // Sharing, import and learning change these in the background. A save that
    // didn't touch them keeps the current ones, rather than undoing those
    // changes from an older copy (and sharing the loss as deletions).
    if let Some(changed) = &changed {
        let touched = |key: &str| changed.iter().any(|c| c == key);
        if !touched("dictionary") {
            settings.dictionary = previous.dictionary.clone();
        }
        if !touched("snippets") {
            settings.snippets = previous.snippets.clone();
        }
    }
    let hotkey_changed = settings.hotkey != previous.hotkey;
    let command_changed = settings.command_hotkey != previous.command_hotkey;
    if !settings.command_hotkey.is_empty() && settings.command_hotkey == settings.hotkey {
        return Err("The command shortcut can't be the dictation hotkey".into());
    }
    let paste_last_changed = settings.paste_last_hotkey != previous.paste_last_hotkey;
    if !settings.paste_last_hotkey.is_empty()
        && (settings.paste_last_hotkey == settings.hotkey
            || settings.paste_last_hotkey == settings.command_hotkey)
    {
        return Err("The paste shortcut is already used by another shortcut".into());
    }

    // Changed first, so a failure shows before anything is saved, and put
    // back if saving fails later on.
    let login_changed = settings.launch_at_login != previous.launch_at_login;
    if login_changed {
        crate::set_launch_at_login(&app, settings.launch_at_login)?;
    }
    let undo_login = || {
        if login_changed {
            let _ = crate::set_launch_at_login(&app, previous.launch_at_login);
        }
    };

    if hotkey_changed {
        let registered =
            state
                .controller
                .register_hotkey(&app, &settings.hotkey, Some(&previous.hotkey));
        if let Err(e) = registered {
            // Put the old hotkey back so dictation keeps working.
            let _ = state
                .controller
                .register_hotkey(&app, &previous.hotkey, None);
            undo_login();
            return Err(e);
        }
    }
    let register_command = |hotkey: &str, previous: &str| {
        command::register_hotkey(&app, &state.controller, hotkey, Some(previous))
    };
    if command_changed
        && let Err(e) = register_command(&settings.command_hotkey, &previous.command_hotkey)
    {
        let _ = register_command(&previous.command_hotkey, &settings.command_hotkey);
        if hotkey_changed {
            let _ =
                state
                    .controller
                    .register_hotkey(&app, &previous.hotkey, Some(&settings.hotkey));
        }
        undo_login();
        return Err(e);
    }
    let register_paste_last = |hotkey: &str, previous: &str| {
        crate::dictation::register_paste_last_hotkey(
            &app,
            &state.controller,
            hotkey,
            Some(previous),
        )
    };
    if paste_last_changed
        && let Err(e) =
            register_paste_last(&settings.paste_last_hotkey, &previous.paste_last_hotkey)
    {
        let _ = register_paste_last(&previous.paste_last_hotkey, &settings.paste_last_hotkey);
        if command_changed {
            let _ = register_command(&previous.command_hotkey, &settings.command_hotkey);
        }
        if hotkey_changed {
            let _ =
                state
                    .controller
                    .register_hotkey(&app, &previous.hotkey, Some(&settings.hotkey));
        }
        undo_login();
        return Err(e);
    }
    if let Err(e) = state.replace_settings(settings.clone()) {
        // Keep the registered hotkeys in step with the saved settings.
        if hotkey_changed {
            let _ =
                state
                    .controller
                    .register_hotkey(&app, &previous.hotkey, Some(&settings.hotkey));
        }
        if command_changed {
            let _ = register_command(&previous.command_hotkey, &settings.command_hotkey);
        }
        if paste_last_changed {
            let _ = register_paste_last(&previous.paste_last_hotkey, &settings.paste_last_hotkey);
        }
        return Err(e);
    }
    if hotkey_changed {
        *state.hotkey_error.lock_unpoisoned() = None;
    }
    if settings.selected_model != previous.selected_model {
        state.warm_up(&app, &settings.selected_model);
    }
    if settings.cleanup != previous.cleanup {
        let builtin =
            settings.cleanup.enabled && settings.cleanup.backend == CleanupBackend::Builtin;
        let restart = !builtin || settings.cleanup.builtin_model != previous.cleanup.builtin_model;
        cleanup::warm_up_server(&settings.cleanup, Some(&previous.cleanup));
        let (app, cleanup) = (app.clone(), settings.cleanup.clone());
        // Stopping waits for a cleanup in progress, so it runs off this thread.
        tauri::async_runtime::spawn_blocking(move || {
            let state = app.state::<AppState>();
            if restart {
                state.cleaner.stop();
            }
            state.cleaner.warm_up(&cleanup, &state.models);
        });
    }
    if settings.show_tray_icon != previous.show_tray_icon
        && let Some(tray) = app.tray_by_id(tray::TRAY_ID)
    {
        let _ = tray.set_visible(settings.show_tray_icon);
    }
    state.controller.send(Event::RefreshPill);
    let _ = app.emit("settings-changed", ());
    // Shares any dictionary or snippet change; does nothing when sharing is off.
    sync::nudge();
    Ok(settings)
}

#[tauri::command]
pub async fn list_input_devices() -> CommandResult<Vec<InputDevice>> {
    Ok(audio::list_input_devices())
}

// ---- Sharing ----

#[tauri::command]
pub async fn get_sync_status() -> CommandResult<sync::Status> {
    Ok(sync::status())
}

/// Asks for a folder to share the dictionary and snippets through. Saving it
/// in settings starts sharing. `None` if the user cancelled.
#[tauri::command]
pub async fn pick_sync_folder(app: AppHandle) -> CommandResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        sync::pick_folder(&app).map(|folder| folder.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_now(app: AppHandle) -> CommandResult<sync::Status> {
    tauri::async_runtime::spawn_blocking(move || sync::sync_now(&app))
        .await
        .map_err(|e| e.to_string())
}

/// Saves the dictionary and snippets to a file. Returns its path, or `None`
/// if the user cancelled.
#[tauri::command]
pub async fn export_library(app: AppHandle) -> CommandResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || sync::export(&app))
        .await
        .map_err(|e| e.to_string())?
        .map(|path| path.map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn import_library(app: AppHandle) -> CommandResult<Option<sync::ImportSummary>> {
    tauri::async_runtime::spawn_blocking(move || sync::import(&app))
        .await
        .map_err(|e| e.to_string())?
}

// ---- Models ----

#[tauri::command]
pub async fn list_models(state: State<'_, AppState>) -> CommandResult<Vec<ModelStatus>> {
    Ok(state.models.statuses())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    loaded: Option<String>,
    loading: Option<String>,
}

#[tauri::command]
pub async fn get_engine_status(state: State<'_, AppState>) -> CommandResult<EngineStatus> {
    let loading = state.engine_loading.lock_unpoisoned().clone();
    // While a model loads the engine is locked, and nothing is usable yet anyway.
    let loaded = state
        .engine
        .try_lock_unpoisoned()
        .and_then(|engine| engine.loaded_model().map(str::to_string));
    Ok(EngineStatus { loaded, loading })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceReport {
    device: DeviceInfo,
    recommendation: Recommendation,
}

#[tauri::command]
pub async fn get_device_info(state: State<'_, AppState>) -> CommandResult<DeviceReport> {
    let device = state.device().clone();
    let language = state.settings().language;
    Ok(DeviceReport {
        recommendation: device::recommend(&device, &language),
        device,
    })
}

#[tauri::command]
pub async fn download_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    accelerator: Option<bool>,
) -> CommandResult<()> {
    let _ = app.emit("models-changed", ());
    let result = state
        .models
        .download(&id, accelerator, |progress| {
            let _ = app.emit("model-progress", progress);
        })
        .await;
    let _ = app.emit("models-changed", ());
    result?;

    if models::find_cleanup(&id).is_some() {
        // The first cleanup model downloaded becomes the one cleanup uses.
        let mut settings = state.settings();
        if settings.cleanup.builtin_model.is_empty() {
            settings.cleanup.builtin_model = id.clone();
            state.replace_settings(settings.clone())?;
            let _ = app.emit("settings-changed", ());
        }
        if settings.cleanup.builtin_model == id {
            state.cleaner.warm_up(&settings.cleanup, &state.models);
        }
        return Ok(());
    }

    // The first downloaded model becomes the selected one.
    let mut settings = state.settings();
    if settings.selected_model.is_empty() {
        settings.selected_model = id.clone();
        state.replace_settings(settings)?;
        let _ = app.emit("settings-changed", ());
    }
    if state.settings().selected_model == id {
        state.warm_up(&app, &id);
    }
    Ok(())
}

/// Replaces models still running from an older download, in the background:
/// Parakeet v3's int8 export with the full-precision one. The old files keep
/// working until the new ones are in place.
pub fn upgrade_models(app: &AppHandle) {
    for model in app.state::<AppState>().models.outdated() {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = upgrade_model(&app, model.id).await {
                eprintln!("[models] couldn't upgrade {}: {e}", model.name);
            }
        });
    }
}

async fn upgrade_model(app: &AppHandle, id: &'static str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let _ = app.emit("models-changed", ());
    let result = state
        .models
        .download(id, None, |progress| {
            let _ = app.emit("model-progress", progress);
        })
        .await;
    let _ = app.emit("models-changed", ());
    result?;
    // Waits for any transcription using the old files to finish.
    let unload_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = unload_app.state::<AppState>();
        let mut engine = state.engine.lock_unpoisoned();
        if engine.loaded_model() == Some(id) {
            engine.unload();
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = app.emit("engine-changed", ());
    if state.settings().selected_model == id {
        state.warm_up(app, id);
    }
    let model = models::find(id).ok_or("Unknown model")?;
    state.models.remove_outdated(model)
}

#[tauri::command]
pub async fn cancel_download(state: State<'_, AppState>, id: String) -> CommandResult<()> {
    state.models.cancel(&id);
    Ok(())
}

#[tauri::command]
pub async fn delete_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<()> {
    if models::find_cleanup(&id).is_some() {
        // Waits for any cleanup using it to finish.
        let cleaner = state.cleaner.clone();
        tauri::async_runtime::spawn_blocking(move || cleaner.stop())
            .await
            .map_err(|e| e.to_string())?;
        state.models.delete(&id)?;
        // So the next cleanup model downloaded is picked up.
        let mut settings = state.settings();
        if settings.cleanup.builtin_model == id {
            settings.cleanup.builtin_model.clear();
            state.replace_settings(settings)?;
            let _ = app.emit("settings-changed", ());
        }
        let _ = app.emit("models-changed", ());
        return Ok(());
    }
    // Waits for any transcription using the model to finish.
    let unload_app = app.clone();
    let unload_id = id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = unload_app.state::<AppState>();
        let mut engine = state.engine.lock_unpoisoned();
        if engine.loaded_model() == Some(unload_id.as_str()) {
            engine.unload();
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    state.models.delete(&id)?;
    let mut settings = state.settings();
    if settings.selected_model == id {
        settings.selected_model.clear();
        state.replace_settings(settings)?;
        let _ = app.emit("settings-changed", ());
    }
    let _ = app.emit("models-changed", ());
    let _ = app.emit("engine-changed", ());
    Ok(())
}

// ---- Cleanup ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupStatus {
    /// Whether this build ships the helper that runs built-in models.
    builtin_available: bool,
    models: Vec<ModelStatus>,
    /// Whether cleanup is on with a model ready, so dictations are actually tidied.
    set_up: bool,
}

#[tauri::command]
pub async fn get_cleanup_status(state: State<'_, AppState>) -> CommandResult<CleanupStatus> {
    Ok(CleanupStatus {
        builtin_available: cleanup::worker_path().is_some(),
        models: state.models.cleanup_statuses(),
        set_up: cleanup::Cleaner::is_set_up(&state.settings().cleanup, &state.models),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerModels {
    models: Vec<String>,
    /// False when the address isn't this computer, so transcripts would leave it.
    is_local: bool,
}

/// Checks a cleanup server's address by listing its models.
#[tauri::command]
pub async fn list_server_models(url: String) -> CommandResult<ServerModels> {
    Ok(ServerModels {
        models: cleanup::server_models(&url).await?,
        is_local: cleanup::is_local(&url),
    })
}

/// Checks hosted API settings by cleaning up a sample sentence. Returns the
/// model's reply.
#[tauri::command]
pub async fn test_cleanup_api(cleanup: CleanupSettings) -> CommandResult<String> {
    tauri::async_runtime::spawn_blocking(move || cleanup::test_api(&cleanup))
        .await
        .map_err(|e| e.to_string())?
}

// ---- History ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToneApp {
    name: &'static str,
    category: AppCategory,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastApp {
    #[serde(flatten)]
    app: platform::FrontApp,
    category: AppCategory,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppTone {
    /// Apps WordFlow already knows, by category.
    built_in: Vec<ToneApp>,
    /// The app dictated into last, so the user can see what it's called.
    last_app: Option<LastApp>,
}

#[tauri::command]
pub async fn get_app_tone(state: State<'_, AppState>) -> CommandResult<AppTone> {
    let rules = state.settings().tone.apps;
    Ok(AppTone {
        built_in: tone::built_in_apps()
            .into_iter()
            .map(|(name, category)| ToneApp { name, category })
            .collect(),
        last_app: tone::last_app().map(|app| LastApp {
            category: tone::classify(&app, &rules),
            app,
        }),
    })
}

#[tauri::command]
pub async fn get_history(state: State<'_, AppState>) -> CommandResult<Vec<HistoryItem>> {
    Ok(state.history.lock_unpoisoned().items().to_vec())
}

#[tauri::command]
pub async fn get_stats(state: State<'_, AppState>) -> CommandResult<Vec<StatsEntry>> {
    Ok(state.history.lock_unpoisoned().stats().to_vec())
}

#[tauri::command]
pub async fn delete_history_item(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<()> {
    state.history.lock_unpoisoned().delete(&id)?;
    let _ = app.emit("history-changed", ());
    Ok(())
}

/// Saves a corrected transcript and learns the misheard words it fixes.
/// Returns the words learned, so the UI can offer to undo them.
#[tauri::command]
pub async fn edit_history_item(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    transcript: String,
) -> CommandResult<Vec<DictionaryEntry>> {
    let previous = state.history.lock_unpoisoned().edit(&id, &transcript)?;
    let _ = app.emit("history-changed", ());
    Ok(previous
        .map(|previous| learn::learn(&app, &previous, transcript.trim()))
        .unwrap_or_default())
}

/// Removes dictionary entries, to undo words that were learned by mistake.
#[tauri::command]
pub async fn forget_words(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> CommandResult<()> {
    state.update_settings(|settings| {
        settings.dictionary.retain(|entry| !ids.contains(&entry.id));
    })?;
    let _ = app.emit("settings-changed", ());
    Ok(())
}

#[tauri::command]
pub async fn clear_history(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    state.history.lock_unpoisoned().clear()?;
    let _ = app.emit("history-changed", ());
    Ok(())
}

/// Returns the WAV bytes of an item's recording for playback.
#[tauri::command]
pub async fn read_history_audio(state: State<'_, AppState>, id: String) -> CommandResult<Response> {
    let path = state
        .history
        .lock_unpoisoned()
        .audio_path(&id)
        .ok_or("The recording for this transcript is missing")?;
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    Ok(Response::new(bytes))
}

#[tauri::command]
pub async fn reveal_history_audio(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> CommandResult<()> {
    let path = state
        .history
        .lock_unpoisoned()
        .audio_path(&id)
        .ok_or("The recording for this transcript is missing")?;
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| e.to_string())
}

// ---- Dictation ----

/// Starts or stops recording, as if the hotkey was pressed in toggle mode.
#[tauri::command]
pub async fn toggle_dictation(state: State<'_, AppState>) -> CommandResult<()> {
    state.controller.send(Event::Toggle);
    Ok(())
}

#[tauri::command]
pub async fn get_dictation_state(state: State<'_, AppState>) -> CommandResult<DictationState> {
    Ok(state.dictation_state.lock_unpoisoned().clone())
}

// ---- Permissions ----

#[tauri::command]
pub async fn get_permissions() -> CommandResult<Vec<Permission>> {
    Ok(platform::permissions())
}

#[tauri::command]
pub async fn request_permission(app: AppHandle, kind: PermissionKind) -> CommandResult<()> {
    // The macOS prompts must be shown from the main thread.
    app.run_on_main_thread(move || platform::request_permission(kind))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_permission_settings(app: AppHandle, kind: PermissionKind) -> CommandResult<()> {
    let url = platform::permission_settings_url(kind).ok_or("No settings page for this")?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

// ---- Windows ----

/// Opens the main window, optionally on a screen such as "settings" or "history".
#[tauri::command]
pub async fn open_main_window(app: AppHandle, route: Option<String>) -> CommandResult<()> {
    tray::open_main_window(&app, route.as_deref());
    Ok(())
}

#[tauri::command]
pub async fn hide_tray_panel(app: AppHandle) -> CommandResult<()> {
    tray::hide_panel(&app);
    Ok(())
}

#[tauri::command]
pub async fn quit_app(app: AppHandle) -> CommandResult<()> {
    app.exit(0);
    Ok(())
}
