use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use database::{setup_database, ChargingHistory};
#[cfg(feature = "ios-monitoring")]
use device::{setup_device_listener, start_device_sender};
use device::{DevicePowerTickEvent, DeviceState};
use event::{DeviceEvent, PowerUpdatedEvent, PreferenceEvent, Theme, WindowLoadedEvent};
use ext::WebviewWindowExt;
use history::{
    setup_history_recorder, ChargingHistoryDetail, ChartHistoryErrorEvent, ChartHistorySaveResult,
    ChartPointEvent, ChartResetEvent, CurrentChart, DeleteAllHistoryResult, HistoryRecordedEvent,
    HistoryRecorderHandle,
};
use local::{setup_sender_with_events, PowerTickEvent};
use menu::setup_menu;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameVibrantDark,
    NSAppearanceNameVibrantLight, NSWindow,
};
#[cfg(debug_assertions)]
use specta_typescript::{BigIntExportBehavior, Typescript};
use sqlx::{Pool, Sqlite};
use tauri::{ActivationPolicy, AppHandle, Manager, RunEvent, State, Window, WindowEvent};
use tauri_plugin_pinia::ManagerExt;
use tauri_specta::{collect_commands, collect_events, Event};
use tpower::ffi::InterfaceType;
use tray_icon::setup_tray_icon;
use util::setup_traffic_light_positioner;

mod database;
pub mod device;
mod event;
mod ext;
mod history;
mod local;
mod menu;
mod tray_icon;
mod util;

static EXIT_FLUSH_PENDING: AtomicBool = AtomicBool::new(false);
static CLOSE_FLUSH_PENDING: AtomicBool = AtomicBool::new(false);
const HISTORY_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

#[tauri::command]
#[specta::specta]
fn open_app(app: AppHandle) {
    let main = app.main_window().unwrap();
    main.show().unwrap();
    main.set_focus().unwrap();
    app.set_activation_policy(ActivationPolicy::Regular)
        .unwrap();
    app.popover_window().unwrap().hide().unwrap();
}

#[tauri::command]
#[specta::specta]
fn open_settings(app: AppHandle) {
    let settings = app.settings_window().unwrap();
    settings.show().unwrap();
    settings.set_focus().unwrap();
}

#[tauri::command]
#[specta::specta]
fn is_main_window_hidden(app: AppHandle) -> bool {
    app.main_window()
        .map(|w| w.is_visible().map(|v| !v).unwrap_or(true))
        .unwrap_or(false)
}

#[tauri::command]
#[specta::specta]
fn get_device_name(
    id: String,
    state: State<DeviceState>,
) -> Option<(String, HashSet<InterfaceType>)> {
    let state = state.read().unwrap();
    let data = state.get(&id);
    data.cloned()
}

#[tauri::command]
#[specta::specta]
fn get_mac_name() -> Option<String> {
    tpower::util::get_mac_name()
}

#[tauri::command]
#[specta::specta]
fn switch_theme(theme: Theme, app: AppHandle) {
    let apprence = match theme {
        Theme::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameVibrantLight }),
        Theme::Dark => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameVibrantDark }),
        Theme::System => None,
    };
    app.webview_windows().values().for_each(|w| unsafe {
        if let Some(w) = (w.ns_window().unwrap() as *mut NSWindow).as_ref() {
            w.setAppearance(apprence.as_deref())
        }
    });
}

#[tauri::command]
#[specta::specta]
async fn get_detail_by_id(
    id: i64,
    db: State<'_, Pool<Sqlite>>,
) -> Result<ChargingHistoryDetail, String> {
    let bytes = database::get_detail_by_id(&db, id).await?;
    let detail = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;

    Ok(detail)
}

#[tauri::command]
#[specta::specta]
async fn delete_history_by_id(
    id: i64,
    recorder: State<'_, HistoryRecorderHandle>,
) -> Result<u64, String> {
    recorder.delete_by_id(id).await
}

#[tauri::command]
#[specta::specta]
async fn delete_all_history(
    recorder: State<'_, HistoryRecorderHandle>,
) -> Result<DeleteAllHistoryResult, String> {
    recorder.delete_all().await
}

#[tauri::command]
#[specta::specta]
async fn retry_history_cleanup(recorder: State<'_, HistoryRecorderHandle>) -> Result<(), String> {
    recorder.retry_cleanup().await
}

#[tauri::command]
#[specta::specta]
async fn set_chart_preferences(
    show_power_usage_chart: bool,
    auto_save_chart: bool,
    app: AppHandle,
    recorder: State<'_, HistoryRecorderHandle>,
) -> Result<(), String> {
    // Persist both values before acknowledging the UI. The settings switches
    // stay disabled until this command resolves, so an immediate tray quit
    // cannot leave the next launch with stale chart policy.
    let pinia = app.pinia();
    pinia
        .set(
            "preference",
            "showPowerUsageChart",
            show_power_usage_chart.into(),
        )
        .map_err(|error| error.to_string())?;
    pinia
        .set("preference", "autoSaveChart", auto_save_chart.into())
        .map_err(|error| error.to_string())?;
    pinia
        .save_now("preference")
        .map_err(|error| error.to_string())?;

    recorder
        .set_preferences(show_power_usage_chart, auto_save_chart)
        .await
}

#[tauri::command]
#[specta::specta]
async fn get_current_chart(
    device_id: String,
    recorder: State<'_, HistoryRecorderHandle>,
) -> Result<CurrentChart, String> {
    recorder.get_current_chart(device_id).await
}

#[tauri::command]
#[specta::specta]
async fn save_current_chart(
    device_id: String,
    recorder: State<'_, HistoryRecorderHandle>,
) -> Result<ChartHistorySaveResult, String> {
    recorder.save_current_chart(device_id).await
}

#[tauri::command]
#[specta::specta]
async fn clear_current_chart(
    device_id: String,
    recorder: State<'_, HistoryRecorderHandle>,
) -> Result<CurrentChart, String> {
    recorder.clear_current_chart(device_id).await
}

#[tauri::command]
#[specta::specta]
async fn get_all_charging_history(
    db: State<'_, Pool<Sqlite>>,
) -> Result<Vec<ChargingHistory>, String> {
    database::get_all_charging_history(&db)
        .await
        .map_err(|e| e.to_string())
}

pub fn create_specta() -> tauri_specta::Builder {
    let builder = tauri_specta::Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            open_app,
            is_main_window_hidden,
            open_settings,
            get_device_name,
            get_mac_name,
            switch_theme,
            get_detail_by_id,
            get_all_charging_history,
            delete_history_by_id,
            delete_all_history,
            retry_history_cleanup,
            set_chart_preferences,
            get_current_chart,
            save_current_chart,
            clear_current_chart
        ])
        .events(collect_events![
            DeviceEvent,
            DevicePowerTickEvent,
            PowerTickEvent,
            PreferenceEvent,
            PowerUpdatedEvent,
            WindowLoadedEvent,
            HistoryRecordedEvent,
            ChartPointEvent,
            ChartResetEvent,
            ChartHistoryErrorEvent,
        ]);

    #[cfg(debug_assertions)]
    builder
        .export(
            Typescript::default()
                .bigint(BigIntExportBehavior::Number)
                .header("// @ts-nocheck"),
            "../src/bindings.ts",
        )
        .expect("Failed to export typescript bindings");

    builder
}

pub fn run() {
    let specta = create_specta();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_pinia::init())
        .plugin(tauri_plugin_nspopover::init())
        .invoke_handler(specta.invoke_handler())
        .manage(DeviceState::default())
        .menu(setup_menu)
        .on_window_event(handle_window_event)
        .setup(move |app| {
            specta.mount_events(app);

            setup_database(app.handle().clone());

            setup_tray_icon(app)?;
            setup_sender_with_events(app);
            #[cfg(feature = "ios-monitoring")]
            {
                start_device_sender(app.app_handle().clone());
                setup_device_listener(app.app_handle().clone());
            }
            setup_history_recorder(app.app_handle().clone());

            setup_traffic_light_positioner(app.main_window().unwrap());

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    app.run(|app, event| match event {
        // Delay cancellable framework exit requests until the history actor
        // has durably flushed. The native macOS Cmd+Q/application-menu Quit
        // uses AppKit terminate directly and intentionally does not save;
        // tray Quit calls the graceful helper below.
        RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            api.prevent_exit();
            request_graceful_exit(app.clone());
        }
        RunEvent::ExitRequested { code: Some(_), .. } => {}
        RunEvent::Reopen {
            has_visible_windows,
            ..
        } if !has_visible_windows => {
            app.main_window().unwrap().show().unwrap();
            app.set_activation_policy(ActivationPolicy::Regular)
                .unwrap();
        }
        _ => (),
    });
}

fn handle_window_event(window: &Window, event: &WindowEvent) {
    if window.label() == "main" {
        match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                if CLOSE_FLUSH_PENDING
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return;
                }

                let window = window.clone();
                let app = window.app_handle().clone();
                let Some(recorder) = app
                    .try_state::<HistoryRecorderHandle>()
                    .map(|state| state.inner().clone())
                else {
                    CLOSE_FLUSH_PENDING.store(false, Ordering::Release);
                    report_flush_failure(
                        &app,
                        "window-close save",
                        "history recorder is unavailable",
                    );
                    return;
                };

                tauri::async_runtime::spawn(async move {
                    match flush_history_with_timeout(recorder).await {
                        Ok(()) => {
                            if let Err(error) = window.hide() {
                                log::error!("failed to hide main window: {error}");
                            }
                            if let Err(error) =
                                app.set_activation_policy(ActivationPolicy::Accessory)
                            {
                                log::error!("failed to change activation policy: {error}");
                            }
                        }
                        Err(error) => {
                            report_flush_failure(&app, "window-close save", &error);
                        }
                    }
                    CLOSE_FLUSH_PENDING.store(false, Ordering::Release);
                });
            }
            WindowEvent::ThemeChanged(theme) => {
                println!("Theme changed to: {}", theme);
            }
            _ => (),
        }
    }
}

/// Start the graceful-exit path used by the tray menu and cancellable Tauri
/// exit requests. `app.exit(0)` is called only after the asynchronous flush
/// succeeds; native Cmd+Q/application-menu Quit intentionally bypasses it.
pub(crate) fn request_graceful_exit(app: AppHandle) {
    if EXIT_FLUSH_PENDING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    let Some(recorder) = app
        .try_state::<HistoryRecorderHandle>()
        .map(|state| state.inner().clone())
    else {
        EXIT_FLUSH_PENDING.store(false, Ordering::Release);
        report_flush_failure(&app, "exit save", "history recorder is unavailable");
        return;
    };

    tauri::async_runtime::spawn(async move {
        match flush_history_with_timeout(recorder).await {
            Ok(()) => app.exit(0),
            Err(error) => {
                EXIT_FLUSH_PENDING.store(false, Ordering::Release);
                report_flush_failure(&app, "exit save", &error);
            }
        }
    });
}

async fn flush_history_with_timeout(recorder: HistoryRecorderHandle) -> Result<(), String> {
    tokio::time::timeout(HISTORY_FLUSH_TIMEOUT, recorder.flush_auto())
        .await
        .map_err(|_| "history save timed out after 10 seconds".to_string())?
}

fn report_flush_failure(app: &AppHandle, operation: &str, message: &str) {
    log::error!("{operation} failed; keeping PowerFlow open: {message}");
    ChartHistoryErrorEvent {
        operation: operation.to_string(),
        message: message.to_string(),
    }
    .emit(app)
    .unwrap_or_else(|error| {
        log::error!("failed to emit ChartHistoryErrorEvent: {error}");
    });

    if let Some(window) = app.main_window() {
        if let Err(error) = window.show() {
            log::error!("failed to show main window after history error: {error}");
        }
        if let Err(error) = window.set_focus() {
            log::error!("failed to focus main window after history error: {error}");
        }
    }
    if let Err(error) = app.set_activation_policy(ActivationPolicy::Regular) {
        log::error!("failed to restore activation policy after history error: {error}");
    }
}
