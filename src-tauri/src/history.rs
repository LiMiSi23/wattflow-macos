use std::{
    collections::{HashMap, VecDeque},
    ops::{Deref, Div},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use specta::Type;
use sqlx::SqlitePool;
use tauri::{async_runtime, AppHandle, Manager};
use tauri_plugin_pinia::ManagerExt;
use tauri_specta::{Event, TypedEvent};
use tokio::sync::{mpsc, oneshot};
use tpower::{
    provider::{NormalizedData, NormalizedResource},
    util::get_mac_name,
};

use crate::{
    database::{
        delete_history_by_id, purge_all_charging_history, retry_history_cleanup,
        upsert_chart_history,
    },
    device::{DevicePowerTickEvent, DeviceState},
    event::PreferenceEvent,
    local::PowerTickEvent,
};

pub const MAX_CHART_POINTS: usize = 100;
pub const AUTO_SAVE_MIN_POINTS: usize = 30;
const LOCAL_TICKS_PER_POINT: u8 = 3;
const LOCAL_DEVICE_ID: &str = "local";

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Deserialize, Type)]
pub struct ChargingHistory {
    pub is_remote: bool,
    pub name: String,
    pub udid: String,
    pub from_level: i32,
    pub end_level: i32,
    pub duration: i64,
    pub timestamp: i64,
    pub adapter_name: String,
    pub detail: ChargingHistoryDetail,
}

#[derive(Serialize, Deserialize, Type)]
pub struct ChargingHistoryDetail {
    pub(crate) avg: HistorySummaryData,
    pub(crate) peak: HistorySummaryData,
    pub(crate) curve: Vec<HistoryCurvePoint>,
}

/// History-specific mirror of `NormalizedData` that deliberately omits only
/// the two battery-percentage fields. All other summary/export telemetry stays
/// available for existing history consumers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistorySummaryData {
    pub system_in: f32,
    pub system_load: f32,
    pub battery_power: f32,
    pub adapter_power: f32,
    pub efficiency_loss: f32,
    pub brightness_power: f32,
    pub heatpipe_power: f32,
    pub temperature: f32,
    pub adapter_watts: f32,
    pub adapter_voltage: f32,
    pub adapter_amperage: f32,
}

impl From<NormalizedData> for HistorySummaryData {
    fn from(data: NormalizedData) -> Self {
        Self {
            system_in: data.system_in,
            system_load: data.system_load,
            battery_power: data.battery_power,
            adapter_power: data.adapter_power,
            efficiency_loss: data.efficiency_loss,
            brightness_power: data.brightness_power,
            heatpipe_power: data.heatpipe_power,
            temperature: data.temperature,
            adapter_watts: data.adapter_watts,
            adapter_voltage: data.adapter_voltage,
            adapter_amperage: data.adapter_amperage,
        }
    }
}

/// History-specific mirror of `NormalizedResource`. It keeps all existing
/// curve/export telemetry except the two percentage fields omitted by its
/// flattened `HistorySummaryData`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCurvePoint {
    pub is_local: bool,
    pub is_charging: bool,
    pub time_remain: Duration,
    pub last_update: i64,
    pub adapter_name: Option<String>,
    pub cycle_count: i32,
    pub current_capacity: i32,
    pub max_capacity: i32,
    #[serde(default)]
    pub design_capacity: i32,
    #[serde(default)]
    pub brightness_power_available: bool,
    #[serde(default)]
    pub heatpipe_power_available: bool,
    #[serde(flatten)]
    pub data: HistorySummaryData,
}

impl From<&NormalizedResource> for HistoryCurvePoint {
    fn from(resource: &NormalizedResource) -> Self {
        Self {
            is_local: resource.is_local,
            is_charging: resource.is_charging,
            time_remain: resource.time_remain,
            last_update: resource.last_update,
            adapter_name: resource.adapter_name.clone(),
            cycle_count: resource.cycle_count,
            current_capacity: resource.current_capacity,
            max_capacity: resource.max_capacity,
            design_capacity: resource.design_capacity,
            brightness_power_available: resource.brightness_power_available,
            heatpipe_power_available: resource.heatpipe_power_available,
            data: resource.data.into(),
        }
    }
}

impl Deref for HistoryCurvePoint {
    type Target = HistorySummaryData;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChartPoint {
    pub sequence: u64,
    /// Wall-clock capture time in Unix milliseconds. Sensor `last_update` is
    /// not a safe chart key because macOS may repeat it or report zero.
    pub captured_at: i64,
    pub data: NormalizedResource,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CurrentChart {
    pub device_id: String,
    pub session_id: String,
    pub points: Vec<ChartPoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ChartHistorySaveResult {
    pub history_id: i64,
    pub session_id: String,
    pub point_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAllHistoryResult {
    pub deleted_count: u64,
    pub cleanup_complete: bool,
    /// Present only when database rows were committed deleted but the
    /// checkpoint/VACUUM/checkpoint cleanup could not be fully confirmed.
    pub cleanup_error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Type, Event)]
pub struct HistoryRecordedEvent;

#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct ChartPointEvent {
    pub device_id: String,
    pub session_id: String,
    pub point: ChartPoint,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct ChartResetEvent {
    pub device_id: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct ChartHistoryErrorEvent {
    pub operation: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum DeviceType {
    Local,
    Remote(String),
}

impl DeviceType {
    fn from_device_id(device_id: String) -> Result<Self, String> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Err("deviceId cannot be empty".to_string());
        }
        Ok(if device_id == LOCAL_DEVICE_ID {
            Self::Local
        } else {
            Self::Remote(device_id.to_string())
        })
    }

    fn device_id(&self) -> &str {
        match self {
            Self::Local => LOCAL_DEVICE_ID,
            Self::Remote(udid) => udid,
        }
    }
}

struct ChartSession {
    session_id: String,
    points: VecDeque<ChartPoint>,
    last_sensor_update: Option<i64>,
    saved_history_id: Option<i64>,
}

impl ChartSession {
    fn new() -> Self {
        Self {
            session_id: new_session_id(),
            points: VecDeque::with_capacity(MAX_CHART_POINTS),
            last_sensor_update: None,
            saved_history_id: None,
        }
    }

    fn rebase_after_history_delete(&mut self) {
        self.session_id = new_session_id();
        self.saved_history_id = None;
    }

    fn current(&self, typ: &DeviceType) -> CurrentChart {
        CurrentChart {
            device_id: typ.device_id().to_string(),
            session_id: self.session_id.clone(),
            points: self.points.iter().cloned().collect(),
        }
    }
}

pub(crate) struct ChartHistorySnapshot {
    pub(crate) session_id: String,
    pub(crate) point_count: usize,
    pub(crate) history: ChargingHistory,
}

struct HistoryRecorder {
    chart_enabled: bool,
    auto_save: bool,
    next_sequence: u64,
    local_tick_count: u8,
    sessions: HashMap<DeviceType, ChartSession>,
}

impl HistoryRecorder {
    fn new(chart_enabled: bool, auto_save: bool) -> Self {
        Self {
            chart_enabled,
            auto_save,
            next_sequence: 0,
            local_tick_count: 0,
            sessions: HashMap::new(),
        }
    }

    fn session_mut(&mut self, typ: &DeviceType) -> &mut ChartSession {
        self.sessions
            .entry(typ.clone())
            .or_insert_with(ChartSession::new)
    }

    fn set_chart_enabled(&mut self, enabled: bool) {
        self.chart_enabled = enabled;
    }

    fn set_auto_save(&mut self, enabled: bool) {
        self.auto_save = enabled;
    }

    fn record(&mut self, typ: DeviceType, data: NormalizedResource) -> Option<ChartPointEvent> {
        if !self.chart_enabled {
            return None;
        }

        match &typ {
            DeviceType::Local => {
                self.local_tick_count = self.local_tick_count.saturating_add(1);
                if self.local_tick_count < LOCAL_TICKS_PER_POINT {
                    return None;
                }
                self.local_tick_count = 0;
            }
            DeviceType::Remote(_) => {
                let session = self.session_mut(&typ);
                if session.last_sensor_update == Some(data.last_update) {
                    return None;
                }
                session.last_sensor_update = Some(data.last_update);
            }
        }

        self.next_sequence = self.next_sequence.wrapping_add(1);
        let point = ChartPoint {
            sequence: self.next_sequence,
            captured_at: unix_time_millis(),
            data,
        };
        let session = self.session_mut(&typ);
        if session.points.len() == MAX_CHART_POINTS {
            session.points.pop_front();
        }
        session.points.push_back(point.clone());

        Some(ChartPointEvent {
            device_id: typ.device_id().to_string(),
            session_id: session.session_id.clone(),
            point,
        })
    }

    fn current_chart(&mut self, typ: &DeviceType) -> CurrentChart {
        self.session_mut(typ).current(typ)
    }

    fn clear_current(&mut self, typ: &DeviceType) -> CurrentChart {
        if matches!(typ, DeviceType::Local) {
            self.local_tick_count = 0;
        }
        let session = ChartSession::new();
        let current = session.current(typ);
        self.sessions.insert(typ.clone(), session);
        current
    }

    fn clear_all(&mut self) -> Vec<CurrentChart> {
        let devices: Vec<_> = self.sessions.keys().cloned().collect();
        devices.iter().map(|typ| self.clear_current(typ)).collect()
    }

    fn mark_saved(&mut self, typ: &DeviceType, history_id: i64) {
        if let Some(session) = self.sessions.get_mut(typ) {
            session.saved_history_id = Some(history_id);
        }
    }

    /// Detach an active in-memory session from a row the user explicitly
    /// deleted. Points remain visible, but the next save uses a new session key
    /// and therefore creates a new row instead of resurrecting the deleted one.
    fn rebase_deleted_history(&mut self, history_id: i64) -> Vec<CurrentChart> {
        self.sessions
            .iter_mut()
            .filter_map(|(typ, session)| {
                (session.saved_history_id == Some(history_id)).then(|| {
                    session.rebase_after_history_delete();
                    session.current(typ)
                })
            })
            .collect()
    }

    fn close_save_devices(&self) -> Vec<DeviceType> {
        if !self.chart_enabled || !self.auto_save {
            return Vec::new();
        }
        self.sessions
            .iter()
            .filter(|(_, session)| session.points.len() >= AUTO_SAVE_MIN_POINTS)
            .map(|(typ, _)| typ.clone())
            .collect()
    }

    fn snapshot(&self, app: &AppHandle, typ: &DeviceType) -> Result<ChartHistorySnapshot, String> {
        if !self.chart_enabled {
            return Err("power usage chart is disabled".to_string());
        }
        let session = self
            .sessions
            .get(typ)
            .ok_or_else(|| "the current chart has no points".to_string())?;
        if session.points.is_empty() {
            return Err("the current chart has no points".to_string());
        }
        summarize_chart_history(app, typ, session)
    }
}

enum HistoryRecorderMessage {
    Power(DeviceType, NormalizedResource),
    SetChartEnabled(bool, oneshot::Sender<Result<(), String>>),
    SetAutoSave(bool, oneshot::Sender<Result<(), String>>),
    SetPreferences {
        chart_enabled: bool,
        auto_save: bool,
        response: oneshot::Sender<Result<(), String>>,
    },
    GetCurrent(DeviceType, oneshot::Sender<Result<CurrentChart, String>>),
    SaveCurrent(
        DeviceType,
        oneshot::Sender<Result<ChartHistorySaveResult, String>>,
    ),
    ClearCurrent(DeviceType, oneshot::Sender<Result<CurrentChart, String>>),
    FlushAuto(oneshot::Sender<Result<(), String>>),
    DeleteById(i64, oneshot::Sender<Result<u64, String>>),
    DeleteAll(oneshot::Sender<Result<DeleteAllHistoryResult, String>>),
    RetryCleanup(oneshot::Sender<Result<(), String>>),
}

#[derive(Clone)]
pub struct HistoryRecorderHandle {
    tx: mpsc::UnboundedSender<HistoryRecorderMessage>,
}

impl HistoryRecorderHandle {
    async fn request<T>(
        &self,
        message: impl FnOnce(oneshot::Sender<Result<T, String>>) -> HistoryRecorderMessage,
    ) -> Result<T, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(message(response_tx))
            .map_err(|error| error.to_string())?;
        response_rx.await.map_err(|error| error.to_string())?
    }

    pub async fn get_current_chart(&self, device_id: String) -> Result<CurrentChart, String> {
        let typ = DeviceType::from_device_id(device_id)?;
        self.request(|response| HistoryRecorderMessage::GetCurrent(typ, response))
            .await
    }

    pub async fn save_current_chart(
        &self,
        device_id: String,
    ) -> Result<ChartHistorySaveResult, String> {
        let typ = DeviceType::from_device_id(device_id)?;
        self.request(|response| HistoryRecorderMessage::SaveCurrent(typ, response))
            .await
    }

    pub async fn clear_current_chart(&self, device_id: String) -> Result<CurrentChart, String> {
        let typ = DeviceType::from_device_id(device_id)?;
        self.request(|response| HistoryRecorderMessage::ClearCurrent(typ, response))
            .await
    }

    pub async fn flush_auto(&self) -> Result<(), String> {
        self.request(HistoryRecorderMessage::FlushAuto).await
    }

    pub async fn set_preferences(
        &self,
        chart_enabled: bool,
        auto_save: bool,
    ) -> Result<(), String> {
        self.request(|response| HistoryRecorderMessage::SetPreferences {
            chart_enabled,
            auto_save,
            response,
        })
        .await
    }

    pub async fn delete_by_id(&self, id: i64) -> Result<u64, String> {
        self.request(|response| HistoryRecorderMessage::DeleteById(id, response))
            .await
    }

    pub async fn delete_all(&self) -> Result<DeleteAllHistoryResult, String> {
        self.request(HistoryRecorderMessage::DeleteAll).await
    }

    pub async fn retry_cleanup(&self) -> Result<(), String> {
        self.request(HistoryRecorderMessage::RetryCleanup).await
    }
}

fn new_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{now:x}-{counter:x}")
}

fn unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn summarize_chart_history(
    app: &AppHandle,
    typ: &DeviceType,
    session: &ChartSession,
) -> Result<ChartHistorySnapshot, String> {
    let first = session
        .points
        .front()
        .ok_or_else(|| "the current chart has no points".to_string())?;
    let last = session
        .points
        .back()
        .ok_or_else(|| "the current chart has no points".to_string())?;

    let name = match typ {
        DeviceType::Local => get_mac_name(),
        DeviceType::Remote(udid) => app
            .state::<DeviceState>()
            .read()
            .ok()
            .and_then(|devices| devices.get(udid).map(|device| device.0.clone())),
    }
    .unwrap_or_default();

    let avg: HistorySummaryData = session
        .points
        .iter()
        .fold(NormalizedData::default(), |acc, point| acc + *point.data)
        .div(session.points.len() as f32)
        .into();
    let peak: HistorySummaryData = session
        .points
        .iter()
        .fold(NormalizedData::default(), |acc, point| {
            acc.max_with(&point.data)
        })
        .into();

    let mut curve = Vec::with_capacity(session.points.len());
    for point in &session.points {
        let mut saved = HistoryCurvePoint::from(&point.data);
        saved.last_update = point.captured_at.div_euclid(1000);
        curve.push(saved);
    }

    let history = ChargingHistory {
        is_remote: matches!(typ, DeviceType::Remote(_)),
        name,
        udid: typ.device_id().to_string(),
        from_level: first.data.battery_level,
        end_level: last.data.battery_level,
        duration: last
            .captured_at
            .saturating_sub(first.captured_at)
            .div_euclid(1000),
        timestamp: first.captured_at.div_euclid(1000),
        adapter_name: last
            .data
            .adapter_name
            .clone()
            .unwrap_or_else(|| "Unknown".to_string()),
        detail: ChargingHistoryDetail { avg, peak, curve },
    };

    Ok(ChartHistorySnapshot {
        session_id: session.session_id.clone(),
        point_count: session.points.len(),
        history,
    })
}

async fn persist_device(
    app: &AppHandle,
    db: &SqlitePool,
    recorder: &mut HistoryRecorder,
    typ: &DeviceType,
) -> Result<ChartHistorySaveResult, String> {
    let snapshot = recorder.snapshot(app, typ)?;
    let history_id = upsert_chart_history(db, &snapshot)
        .await
        .map_err(|error| error.to_string())?;
    recorder.mark_saved(typ, history_id);
    HistoryRecordedEvent.emit(app).unwrap_or_else(|error| {
        log::error!("failed to emit HistoryRecordedEvent: {error}");
    });

    Ok(ChartHistorySaveResult {
        history_id,
        session_id: snapshot.session_id,
        point_count: snapshot.point_count,
    })
}

fn emit_current_chart(app: &AppHandle, current: CurrentChart) {
    ChartResetEvent {
        device_id: current.device_id.clone(),
        session_id: current.session_id.clone(),
    }
    .emit(app)
    .unwrap_or_else(|error| {
        log::error!("failed to emit ChartResetEvent: {error}");
    });
    for point in current.points {
        ChartPointEvent {
            device_id: current.device_id.clone(),
            session_id: current.session_id.clone(),
            point,
        }
        .emit(app)
        .unwrap_or_else(|error| {
            log::error!("failed to emit rebased ChartPointEvent: {error}");
        });
    }
}

fn emit_history_error(app: &AppHandle, operation: &str, error: &str) {
    log::error!("chart history {operation} failed: {error}");
    ChartHistoryErrorEvent {
        operation: operation.to_string(),
        message: error.to_string(),
    }
    .emit(app)
    .unwrap_or_else(|emit_error| {
        log::error!("failed to emit ChartHistoryErrorEvent: {emit_error}");
    });
}

async fn persist_auto_devices(
    app: &AppHandle,
    db: &SqlitePool,
    recorder: &mut HistoryRecorder,
    devices: Vec<DeviceType>,
    operation: &str,
) -> Result<(), String> {
    let mut errors = Vec::new();
    for typ in devices {
        if let Err(error) = persist_device(app, db, recorder, &typ).await {
            emit_history_error(app, operation, &error);
            errors.push(format!("{}: {error}", typ.device_id()));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn spawn_history_recorder(
    app: AppHandle,
    mut rx: mpsc::UnboundedReceiver<HistoryRecorderMessage>,
    chart_enabled: bool,
    auto_save: bool,
) {
    async_runtime::spawn(async move {
        let db = app.state::<SqlitePool>();
        let mut recorder = HistoryRecorder::new(chart_enabled, auto_save);

        while let Some(message) = rx.recv().await {
            match message {
                HistoryRecorderMessage::Power(typ, data) => {
                    if let Some(event) = recorder.record(typ, data) {
                        event.emit(&app).unwrap_or_else(|error| {
                            log::error!("failed to emit ChartPointEvent: {error}");
                        });
                    }
                }
                HistoryRecorderMessage::SetChartEnabled(enabled, response) => {
                    recorder.set_chart_enabled(enabled);
                    let _ = response.send(Ok(()));
                }
                HistoryRecorderMessage::SetAutoSave(enabled, response) => {
                    recorder.set_auto_save(enabled);
                    let _ = response.send(Ok(()));
                }
                HistoryRecorderMessage::SetPreferences {
                    chart_enabled,
                    auto_save,
                    response,
                } => {
                    recorder.set_chart_enabled(chart_enabled);
                    recorder.set_auto_save(auto_save);
                    let _ = response.send(Ok(()));
                }
                HistoryRecorderMessage::GetCurrent(typ, response) => {
                    let _ = response.send(Ok(recorder.current_chart(&typ)));
                }
                HistoryRecorderMessage::SaveCurrent(typ, response) => {
                    let result = persist_device(&app, &db, &mut recorder, &typ).await;
                    if let Err(error) = &result {
                        emit_history_error(&app, "manual save", error);
                    }
                    let _ = response.send(result);
                }
                HistoryRecorderMessage::ClearCurrent(typ, response) => {
                    let current = recorder.clear_current(&typ);
                    ChartResetEvent {
                        device_id: current.device_id.clone(),
                        session_id: current.session_id.clone(),
                    }
                    .emit(&app)
                    .unwrap_or_else(|error| {
                        log::error!("failed to emit ChartResetEvent: {error}");
                    });
                    let _ = response.send(Ok(current));
                }
                HistoryRecorderMessage::FlushAuto(response) => {
                    let devices = recorder.close_save_devices();
                    let result =
                        persist_auto_devices(&app, &db, &mut recorder, devices, "close/exit save")
                            .await;
                    let _ = response.send(result);
                }
                HistoryRecorderMessage::DeleteById(id, response) => {
                    let result = delete_history_by_id(&db, id)
                        .await
                        .map(|result| {
                            let rows_deleted = result.rows_affected();
                            if rows_deleted > 0 {
                                for current in recorder.rebase_deleted_history(id) {
                                    emit_current_chart(&app, current);
                                }
                            }
                            rows_deleted
                        })
                        .map_err(|error| error.to_string());
                    let _ = response.send(result);
                }
                HistoryRecorderMessage::DeleteAll(response) => {
                    let result = match purge_all_charging_history(&db).await {
                        Err(error) => Err(error.to_string()),
                        Ok(result) => {
                            // Only clear current sessions after the DELETE
                            // transaction is confirmed committed. A later
                            // cleanup warning does not undo that deletion.
                            for current in recorder.clear_all() {
                                ChartResetEvent {
                                    device_id: current.device_id,
                                    session_id: current.session_id,
                                }
                                .emit(&app)
                                .unwrap_or_else(|error| {
                                    log::error!("failed to emit ChartResetEvent: {error}");
                                });
                            }

                            if let Some(error) = &result.cleanup_error {
                                emit_history_error(
                                    &app,
                                    "delete-all cleanup",
                                    &format!(
                                        "history was deleted, but secure disk cleanup did not complete: {error}"
                                    ),
                                );
                            }
                            Ok(DeleteAllHistoryResult {
                                deleted_count: result.rows_deleted,
                                cleanup_complete: result.cleanup_error.is_none(),
                                cleanup_error: result.cleanup_error,
                            })
                        }
                    };
                    let _ = response.send(result);
                }
                HistoryRecorderMessage::RetryCleanup(response) => {
                    // Deliberately do not touch `recorder`: this retry only
                    // checkpoints/vacuums the database and must preserve both
                    // active chart sessions and any history recorded since the
                    // original delete-all.
                    let result = retry_history_cleanup(&db)
                        .await
                        .map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        emit_history_error(&app, "history cleanup retry", error);
                    }
                    let _ = response.send(result);
                }
            }
        }
    });
}

pub fn setup_history_recorder(app: AppHandle) {
    let chart_enabled = app
        .pinia()
        .try_get::<bool>("preference", "showPowerUsageChart")
        .unwrap_or(true);
    let auto_save = app
        .pinia()
        .try_get::<bool>("preference", "autoSaveChart")
        .unwrap_or(false);
    let (tx, rx) = mpsc::unbounded_channel();
    app.manage(HistoryRecorderHandle { tx: tx.clone() });

    let tx_cloned = tx.clone();
    PowerTickEvent::listen(&app, move |TypedEvent { payload, .. }| {
        tx_cloned
            .send(HistoryRecorderMessage::Power(
                DeviceType::Local,
                payload.data,
            ))
            .unwrap_or_else(|error| {
                log::error!("failed to enqueue PowerTickEvent: {error}");
            });
    });

    let tx_cloned = tx.clone();
    DevicePowerTickEvent::listen(&app, move |TypedEvent { payload, .. }| {
        tx_cloned
            .send(HistoryRecorderMessage::Power(
                DeviceType::Remote(payload.udid),
                payload.data,
            ))
            .unwrap_or_else(|error| {
                log::error!("failed to enqueue DevicePowerTickEvent: {error}");
            });
    });

    let tx_cloned = tx;
    PreferenceEvent::listen(&app, move |event| {
        enum PreferenceUpdate {
            ChartEnabled(bool),
            AutoSave(bool),
        }

        let update = match event.payload {
            PreferenceEvent::ShowPowerUsageChart(enabled) => {
                Some(PreferenceUpdate::ChartEnabled(enabled))
            }
            PreferenceEvent::AutoSaveChart(enabled) => Some(PreferenceUpdate::AutoSave(enabled)),
            _ => None,
        };
        if let Some(update) = update {
            let (response_tx, response_rx) = oneshot::channel();
            let message = match update {
                PreferenceUpdate::ChartEnabled(enabled) => {
                    HistoryRecorderMessage::SetChartEnabled(enabled, response_tx)
                }
                PreferenceUpdate::AutoSave(enabled) => {
                    HistoryRecorderMessage::SetAutoSave(enabled, response_tx)
                }
            };
            if let Err(error) = tx_cloned.send(message) {
                log::error!("failed to update chart history preference: {error}");
                return;
            }

            // Enqueueing above is synchronous, so a later FlushAuto is ordered
            // after this preference update. Await the actor acknowledgment in a
            // task so failures are observed without blocking Tauri's event loop.
            async_runtime::spawn(async move {
                match response_rx.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        log::error!("chart history preference update failed: {error}");
                    }
                    Err(error) => {
                        log::error!("chart history preference acknowledgment failed: {error}");
                    }
                }
            });
        }
    });

    spawn_history_recorder(app.clone(), rx, chart_enabled, auto_save);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tpower::provider::{NormalizedData, NormalizedResource};

    use super::{
        ChargingHistoryDetail, DeviceType, HistoryCurvePoint, HistoryRecorder, HistorySummaryData,
        AUTO_SAVE_MIN_POINTS, MAX_CHART_POINTS,
    };

    fn sample(value: i64) -> NormalizedResource {
        NormalizedResource {
            last_update: value,
            data: NormalizedData {
                battery_level: (value % 100) as i32,
                system_load: value as f32,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn json_keys(value: &serde_json::Value) -> BTreeSet<String> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn persisted_history_dto_omits_exactly_battery_percentages_and_raw() {
        let resource = sample(42);

        let full_summary = serde_json::to_value(resource.data).unwrap();
        let compact_summary =
            serde_json::to_value(HistorySummaryData::from(resource.data)).unwrap();
        let mut expected_summary_keys = json_keys(&full_summary);
        expected_summary_keys.remove("batteryLevel");
        expected_summary_keys.remove("absoluteBatteryLevel");
        assert_eq!(json_keys(&compact_summary), expected_summary_keys);

        let full_curve = serde_json::to_value(&resource).unwrap();
        let compact_curve = serde_json::to_value(HistoryCurvePoint::from(&resource)).unwrap();
        let mut expected_curve_keys = json_keys(&full_curve);
        expected_curve_keys.remove("batteryLevel");
        expected_curve_keys.remove("absoluteBatteryLevel");
        assert_eq!(json_keys(&compact_curve), expected_curve_keys);

        let detail = ChargingHistoryDetail {
            avg: HistorySummaryData::from(resource.data),
            peak: HistorySummaryData::from(resource.data),
            curve: vec![HistoryCurvePoint::from(&resource)],
        };
        assert_eq!(
            json_keys(&serde_json::to_value(detail).unwrap()),
            BTreeSet::from(["avg".to_string(), "curve".to_string(), "peak".to_string()])
        );
    }

    #[test]
    fn chart_disabled_stops_sampling_without_discarding_existing_session() {
        let mut recorder = HistoryRecorder::new(true, false);
        recorder.record(DeviceType::Local, sample(1));
        recorder.record(DeviceType::Local, sample(1));
        recorder.record(DeviceType::Local, sample(1)).unwrap();
        let session_id = recorder.current_chart(&DeviceType::Local).session_id;

        recorder.set_chart_enabled(false);
        assert!(recorder.record(DeviceType::Local, sample(2)).is_none());
        let chart = recorder.current_chart(&DeviceType::Local);
        assert_eq!(chart.session_id, session_id);
        assert_eq!(chart.points.len(), 1);
    }

    #[test]
    fn current_chart_is_a_one_hundred_point_ring() {
        let mut recorder = HistoryRecorder::new(true, false);
        for value in 1..=(MAX_CHART_POINTS as i64 + 7) {
            for _ in 0..3 {
                recorder.record(DeviceType::Local, sample(value));
            }
        }

        let chart = recorder.current_chart(&DeviceType::Local);
        assert_eq!(chart.points.len(), MAX_CHART_POINTS);
        assert_eq!(chart.points.first().unwrap().data.last_update, 8);
        assert_eq!(
            chart.points.last().unwrap().data.last_update,
            MAX_CHART_POINTS as i64 + 7
        );
    }

    #[test]
    fn local_chart_accepts_only_every_third_tick() {
        let mut recorder = HistoryRecorder::new(true, false);
        assert!(recorder.record(DeviceType::Local, sample(1)).is_none());
        assert!(recorder.record(DeviceType::Local, sample(2)).is_none());
        assert!(recorder.record(DeviceType::Local, sample(3)).is_some());
        assert_eq!(recorder.current_chart(&DeviceType::Local).points.len(), 1);
    }

    #[test]
    fn remote_chart_deduplicates_repeated_sensor_updates() {
        let mut recorder = HistoryRecorder::new(true, false);
        let remote = DeviceType::Remote("phone".to_string());
        assert!(recorder.record(remote.clone(), sample(7)).is_some());
        assert!(recorder.record(remote.clone(), sample(7)).is_none());
        assert!(recorder.record(remote.clone(), sample(8)).is_some());
        assert_eq!(recorder.current_chart(&remote).points.len(), 2);
    }

    #[test]
    fn close_auto_save_requires_thirty_visible_points() {
        let mut recorder = HistoryRecorder::new(true, true);
        for value in 1..AUTO_SAVE_MIN_POINTS {
            for _ in 0..3 {
                recorder.record(DeviceType::Local, sample(value as i64));
            }
        }
        assert!(recorder.close_save_devices().is_empty());
        for _ in 0..3 {
            recorder.record(DeviceType::Local, sample(AUTO_SAVE_MIN_POINTS as i64));
        }
        assert_eq!(recorder.close_save_devices(), vec![DeviceType::Local]);

        recorder.set_auto_save(false);
        assert!(recorder.close_save_devices().is_empty());
        recorder.set_auto_save(true);
        recorder.set_chart_enabled(false);
        assert!(recorder.close_save_devices().is_empty());
    }

    #[test]
    fn clearing_only_replaces_the_selected_device_session() {
        let mut recorder = HistoryRecorder::new(true, false);
        let remote = DeviceType::Remote("phone".to_string());
        for _ in 0..3 {
            recorder.record(DeviceType::Local, sample(1));
        }
        recorder.record(remote.clone(), sample(2));
        let old_local = recorder.current_chart(&DeviceType::Local).session_id;
        let old_remote = recorder.current_chart(&remote).session_id;

        let cleared = recorder.clear_current(&DeviceType::Local);
        assert_ne!(cleared.session_id, old_local);
        assert!(cleared.points.is_empty());
        assert_eq!(recorder.current_chart(&remote).session_id, old_remote);
        assert_eq!(recorder.current_chart(&remote).points.len(), 1);
    }

    #[test]
    fn deleting_all_history_discards_every_cached_chart_session() {
        let mut recorder = HistoryRecorder::new(true, true);
        let remote = DeviceType::Remote("phone".to_string());
        for value in 1..=3 {
            recorder.record(DeviceType::Local, sample(value));
        }
        recorder.record(remote.clone(), sample(10));

        let resets = recorder.clear_all();
        assert_eq!(resets.len(), 2);
        assert!(resets.iter().all(|chart| chart.points.is_empty()));
        assert!(recorder.current_chart(&DeviceType::Local).points.is_empty());
        assert!(recorder.current_chart(&remote).points.is_empty());
        assert!(recorder.close_save_devices().is_empty());
    }

    #[test]
    fn deleting_active_saved_row_rebases_session_without_losing_visible_points() {
        let mut recorder = HistoryRecorder::new(true, false);
        for value in 1..=3 {
            recorder.record(DeviceType::Local, sample(value));
        }
        let before = recorder.current_chart(&DeviceType::Local);
        assert_eq!(before.points.len(), 1);
        recorder.mark_saved(&DeviceType::Local, 42);

        assert!(recorder.rebase_deleted_history(7).is_empty());
        assert_eq!(
            recorder.current_chart(&DeviceType::Local).session_id,
            before.session_id
        );

        let rebased = recorder.rebase_deleted_history(42);
        assert_eq!(rebased.len(), 1);
        assert_ne!(rebased[0].session_id, before.session_id);
        assert_eq!(rebased[0].points.len(), before.points.len());
        assert_eq!(rebased[0].points[0].sequence, before.points[0].sequence);
        assert_eq!(recorder.sessions[&DeviceType::Local].saved_history_id, None);
    }

    #[test]
    fn legacy_v0_2_2_detail_payload_deserializes_with_new_sensor_defaults() {
        let payload = include_bytes!("../test-data/legacy_history_detail_v0_2_2.json");
        let detail: super::ChargingHistoryDetail = serde_json::from_slice(payload).unwrap();

        assert_eq!(detail.curve.len(), 1);
        assert_eq!(detail.avg.system_load, 18.25);
        assert_eq!(detail.peak.adapter_watts, 67.0);
        assert_eq!(detail.curve[0].last_update, 1_735_689_600);
        assert_eq!(detail.curve[0].system_in, 44.5);
        assert_eq!(detail.curve[0].system_load, 18.25);
        assert_eq!(detail.curve[0].battery_power, 25.1);
        assert!(detail.curve[0].is_charging);
        assert_eq!(detail.curve[0].current_capacity, 3500);
        assert_eq!(detail.curve[0].temperature, 34.8);
        assert_eq!(detail.curve[0].brightness_power, 2.2);

        let compact = serde_json::to_value(&detail).unwrap();
        assert!(compact.get("raw").is_none());
        assert!(compact["curve"][0].get("batteryLevel").is_none());
        assert!(compact["curve"][0].get("absoluteBatteryLevel").is_none());
        assert_eq!(compact["curve"][0]["currentCapacity"], 3500);
        assert!(compact["curve"][0].get("brightnessPower").is_some());
    }
}
