use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{async_runtime, AppHandle, Manager, Runtime};
use tauri_plugin_pinia::ManagerExt;
use tauri_specta::Event;
use tokio::{select, sync::mpsc, time};
use tpower::{
    ffi::smc::{SMCConnection, SMCPowerData, SMCReadSensor},
    provider::{duration_from_minutes, get_mac_ioreg, NormalizedData, NormalizedResource},
};

use crate::event::{PowerUpdatedEvent, PreferenceEvent, StatusBarItem, WindowLoadedEvent};

pub enum SenderMessage {
    ImmediateSend,
    ChangeInterval(Duration),
    ChangeStatusBarItem(StatusBarItem),
    StatusBarShowCharging(bool),
}

pub fn status_bar_text(
    smc: &SMCPowerData,
    status_bar_item: &StatusBarItem,
    show_charging: bool,
) -> f32 {
    if smc.is_charging() && show_charging {
        return smc.delivery_rate;
    }
    match status_bar_item {
        StatusBarItem::System => smc.system_total,
        StatusBarItem::Screen => smc.brightness,
        StatusBarItem::Heatpipe => smc.heatpipe,
    }
}

impl PowerUpdatedEvent {
    pub fn new(value: f32) -> Self {
        let value = if value.is_finite() { value } else { 0.0 };
        Self(format!("{:.1} w", value))
    }

    pub fn new_with(
        smc: &SMCPowerData,
        status_bar_item: &StatusBarItem,
        show_charging: bool,
    ) -> Self {
        Self::new(status_bar_text(smc, status_bar_item, show_charging))
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Event, Type)]
#[serde(rename_all = "camelCase")]
pub struct PowerTickEvent {
    pub data: NormalizedResource,
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn smc_only_resource(smc: &SMCPowerData) -> NormalizedResource {
    let delivery_rate = finite_or_zero(smc.delivery_rate);
    let system_total = finite_or_zero(smc.system_total);
    let battery_rate = finite_or_zero(smc.battery_rate);

    NormalizedResource {
        is_local: true,
        is_charging: smc.is_charging(),
        time_remain: duration_from_minutes(if smc.is_charging() {
            smc.time_to_full
        } else {
            smc.time_to_empty
        }),
        data: NormalizedData {
            system_in: delivery_rate,
            system_load: system_total,
            battery_power: battery_rate.max(delivery_rate - system_total),
            adapter_power: delivery_rate,
            brightness_power: finite_or_zero(smc.brightness),
            heatpipe_power: finite_or_zero(smc.heatpipe),
            temperature: finite_or_zero(smc.temperature),
            ..Default::default()
        },
        brightness_power_available: smc.brightness_available,
        heatpipe_power_available: smc.heatpipe_available,
        ..Default::default()
    }
}

fn emit_local_power_tick<R: Runtime>(
    app: &AppHandle<R>,
    smc: &SMCPowerData,
    status_bar_item: &StatusBarItem,
    show_charging: bool,
) {
    if let Err(error) = PowerUpdatedEvent::new_with(smc, status_bar_item, show_charging).emit(app) {
        log::warn!("failed to emit PowerUpdatedEvent: {error}");
    }

    let data = match get_mac_ioreg() {
        Ok(ioreg) => (&ioreg, smc).into(),
        Err(error) => {
            log::warn!("failed to read mac ioreg; using SMC-only data: {error:?}");
            smc_only_resource(smc)
        }
    };

    if let Err(error) = (PowerTickEvent { data }).emit(app) {
        log::warn!("failed to emit PowerTickEvent: {error}");
    }
}

pub fn start_sender<R: Runtime>(
    app: &impl Manager<R>,
    mut rx: mpsc::Receiver<SenderMessage>,
) -> async_runtime::JoinHandle<()> {
    let app = app.app_handle().clone();
    let mut smc_conn = match SMCConnection::new("AppleSMC") {
        Ok(connection) => connection,
        Err(error) => {
            log::error!("failed to open AppleSMC connection ({error}); local monitoring disabled");
            return async_runtime::spawn(async move { while rx.recv().await.is_some() {} });
        }
    };

    let mut timer = time::interval(Duration::from_millis(
        app.pinia()
            .try_get::<u64>("preference", "updateInterval")
            .unwrap_or(2000),
    ));
    let mut status_bar_item = app
        .pinia()
        .try_get::<StatusBarItem>("preference", "statusBarItem")
        .unwrap_or(StatusBarItem::System);
    let mut show_charging = app
        .pinia()
        .try_get::<bool>("preference", "showCharging")
        .unwrap_or(true);

    async_runtime::spawn(async move {
        loop {
            select! {
                _ = timer.tick() => {
                    let smc = smc_conn.read_sensor();
                    emit_local_power_tick(&app, &smc, &status_bar_item, show_charging);
                }
                Some(msg) = rx.recv() => match msg {
                    SenderMessage::ImmediateSend => {
                        let smc = smc_conn.read_sensor();
                        emit_local_power_tick(&app, &smc, &status_bar_item, show_charging);
                    },
                    SenderMessage::ChangeInterval(interval) => {
                        timer = time::interval(if interval < Duration::from_millis(500) {
                            log::warn!("interval is too small, set to 500ms");
                            Duration::from_millis(500)
                        } else {
                            interval
                        });
                    },
                    SenderMessage::ChangeStatusBarItem(item) => {
                        status_bar_item = item;
                        if let Err(e) = PowerUpdatedEvent::new_with(&smc_conn.read_sensor(), &status_bar_item, show_charging)
                            .emit(&app)
                        {
                            log::warn!("failed to emit PowerUpdatedEvent: {e}");
                        }
                    },
                    SenderMessage::StatusBarShowCharging(show) => {
                        show_charging = show;
                        if let Err(e) = PowerUpdatedEvent::new_with(&smc_conn.read_sensor(), &status_bar_item, show_charging)
                            .emit(&app)
                        {
                            log::warn!("failed to emit PowerUpdatedEvent: {e}");
                        }
                    }
                }
            }
        }
    })
}

pub fn setup_sender_with_events<R: Runtime>(app: &impl Manager<R>) {
    let app = app.app_handle();
    let (sender_tx, rx) = mpsc::channel(10);
    start_sender(app, rx);

    // send an immediate update when the main window is loaded
    let tx = sender_tx.clone();
    WindowLoadedEvent::listen(app, move |_| {
        let tx = tx.clone();
        async_runtime::spawn(async move {
            if let Err(e) = tx.send(SenderMessage::ImmediateSend).await {
                log::warn!("failed to send ImmediateSend: {e}");
            }
        });
    });

    let tx = sender_tx.clone();
    PreferenceEvent::listen(app, move |event| {
        if let Some(msg) = match event.payload {
            PreferenceEvent::UpdateInterval(interval) => Some(SenderMessage::ChangeInterval(
                Duration::from_millis(interval.into()),
            )),
            PreferenceEvent::StatusBarItem(item) => Some(SenderMessage::ChangeStatusBarItem(item)),
            PreferenceEvent::StatusBarShowCharging(show) => {
                Some(SenderMessage::StatusBarShowCharging(show))
            }
            PreferenceEvent::Language(_) => {
                // No need to send, perform some menu refreshing
                None
            }
            _ => None,
        } {
            let tx = tx.clone();
            async_runtime::spawn(async move {
                if let Err(e) = tx.send(msg).await {
                    log::warn!("failed to send SenderMessage: {e}");
                }
            });
        }
    });
}
