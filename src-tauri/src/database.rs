use std::fs;

use serde::{Deserialize, Serialize};
use specta::Type;
use sqlx::{
    migrate, query, query_as,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePoolOptions, SqliteQueryResult},
    Acquire, SqlitePool,
};
use tauri::{
    async_runtime::{self},
    AppHandle, Manager,
};
use tokio::task::block_in_place;

use crate::history;

static DEFAULT_DATABASE_NAME: &str = "db.sqlite";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPurgeResult {
    pub rows_deleted: u64,
    /// `None` means the post-commit checkpoint/VACUUM/checkpoint sequence also
    /// completed. `Some` means deletion was committed, but physical cleanup
    /// was not fully confirmed and may be retried on a later delete-all.
    pub cleanup_error: Option<String>,
}

#[derive(Debug, sqlx::FromRow, Type, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargingHistory {
    id: i64,
    from_level: i64,
    end_level: i64,
    charging_time: i64,
    timestamp: i64,
    name: String,
    udid: String,
    is_remote: i64,
    adapter_name: String,
    history_kind: String,
    point_count: i64,
}

pub async fn get_all_charging_history(
    conn: &SqlitePool,
) -> Result<Vec<ChargingHistory>, sqlx::Error> {
    query_as::<_, ChargingHistory>(
        "SELECT id, from_level, end_level, charging_time, timestamp, name, udid, \
         is_remote, adapter_name, history_kind, point_count \
         FROM charging_histories ORDER BY timestamp DESC, id DESC",
    )
    .fetch_all(conn)
    .await
}

pub async fn get_detail_by_id(conn: &SqlitePool, id: i64) -> Result<Vec<u8>, String> {
    query!("SELECT detail FROM charging_histories WHERE id = ?", id)
        .fetch_one(conn)
        .await
        .map(|v| v.detail)
        .map_err(|e| e.to_string())
}

pub async fn delete_history_by_id(
    conn: &SqlitePool,
    id: i64,
) -> Result<SqliteQueryResult, sqlx::Error> {
    query!("DELETE FROM charging_histories WHERE id = ?", id)
        .execute(conn)
        .await
}

pub async fn purge_all_charging_history(
    pool: &SqlitePool,
) -> Result<HistoryPurgeResult, sqlx::Error> {
    purge_all_charging_history_with_cleanup(pool, "PRAGMA wal_checkpoint(TRUNCATE)", "VACUUM").await
}

/// Retry only the physical SQLite cleanup that follows a committed delete-all.
/// This intentionally performs no DELETE and can therefore run after new
/// history has already been recorded without removing it.
pub async fn retry_history_cleanup(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    retry_history_cleanup_with_statements(pool, "PRAGMA wal_checkpoint(TRUNCATE)", "VACUUM").await
}

/// Rewrite legacy full-telemetry chart blobs into the compact history format.
/// Charging records are intentionally excluded. A row is marked version 1 in
/// the same transaction as its rewritten blob, so interrupted work is retried
/// safely on the next launch.
pub async fn compact_chart_history_details(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let rows: Vec<(i64, Vec<u8>)> = query_as(
        "SELECT id, detail FROM charging_histories \
         WHERE history_kind = 'chart' AND detail_format_version < 1 \
         ORDER BY id",
    )
    .fetch_all(&mut *transaction)
    .await?;
    let mut updated = 0;

    for (id, bytes) in rows {
        let detail: history::ChargingHistoryDetail = match serde_json::from_slice(&bytes) {
            Ok(detail) => detail,
            Err(error) => {
                // Keep the version at zero so a future app version or repaired
                // payload can retry. One malformed row must not block valid
                // chart histories from being compacted.
                log::warn!("skipping unreadable chart history {id} during compaction: {error}");
                continue;
            }
        };
        let compact = serde_json::to_vec(&detail).map_err(|error| {
            sqlx::Error::Protocol(format!("serialize compact chart history {id}: {error}"))
        })?;
        updated += query(
            "UPDATE charging_histories \
             SET detail = ?, detail_format_version = 1 \
             WHERE id = ? AND history_kind = 'chart' AND detail_format_version < 1",
        )
        .bind(compact)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
    }

    transaction.commit().await?;
    Ok(updated)
}

async fn retry_history_cleanup_with_statements(
    pool: &SqlitePool,
    checkpoint_statement: &str,
    vacuum_statement: &str,
) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    prepare_history_cleanup(&mut conn).await?;
    finish_history_cleanup(&mut conn, checkpoint_statement, vacuum_statement).await
}

async fn purge_all_charging_history_with_cleanup(
    pool: &SqlitePool,
    checkpoint_statement: &str,
    vacuum_statement: &str,
) -> Result<HistoryPurgeResult, sqlx::Error> {
    let mut conn = pool.acquire().await?;

    // Overwrite deleted payloads before SQLite releases their pages. VACUUM then
    // removes the free pages so curve/raw detail blobs cannot remain orphaned in
    // the database file after the user chooses Delete All.
    prepare_history_cleanup(&mut conn).await?;

    let mut transaction = conn.begin().await?;
    let result = query("DELETE FROM charging_histories")
        .execute(&mut *transaction)
        .await?;
    query("DELETE FROM sqlite_sequence WHERE name = 'charging_histories'")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;

    // A cleanup failure after this point must not be returned as an ordinary
    // delete failure: the rows have already been committed as deleted. Keeping
    // the two outcomes distinct lets the actor clear its in-memory sessions and
    // lets the UI report an accurate physical-cleanup warning.
    let cleanup_error = finish_history_cleanup(&mut conn, checkpoint_statement, vacuum_statement)
        .await
        .err()
        .map(|error| error.to_string());

    Ok(HistoryPurgeResult {
        rows_deleted: result.rows_affected(),
        cleanup_error,
    })
}

async fn prepare_history_cleanup(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    query("PRAGMA busy_timeout = 5000")
        .execute(&mut *conn)
        .await?;
    query("PRAGMA secure_delete = ON")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn run_wal_checkpoint(
    conn: &mut SqliteConnection,
    statement: &str,
) -> Result<(), sqlx::Error> {
    let checkpoint: (i64, i64, i64) = query_as(statement).fetch_one(conn).await?;
    if checkpoint.0 != 0 {
        return Err(sqlx::Error::Protocol(format!(
            "checkpoint remained busy: {checkpoint:?}"
        )));
    }

    Ok(())
}

async fn finish_history_cleanup(
    conn: &mut SqliteConnection,
    checkpoint_statement: &str,
    vacuum_statement: &str,
) -> Result<(), sqlx::Error> {
    let mut errors = Vec::new();

    if let Err(error) = run_wal_checkpoint(conn, checkpoint_statement).await {
        errors.push(format!("WAL checkpoint before VACUUM failed: {error}"));
    }

    if let Err(error) = query(vacuum_statement).execute(&mut *conn).await {
        errors.push(format!("VACUUM failed: {error}"));
    }

    // Always attempt the final checkpoint, even if the first checkpoint or
    // VACUUM failed, so successfully scrubbed WAL frames are still truncated.
    if let Err(error) = run_wal_checkpoint(conn, checkpoint_statement).await {
        errors.push(format!("WAL checkpoint after VACUUM failed: {error}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(sqlx::Error::Protocol(errors.join("; ")))
    }
}

/// Insert the first snapshot of a chart session or replace the snapshot for the
/// same session. A chart session deliberately owns one database row: manual,
/// window-close, and exit saves all update it until the user clears the current
/// chart and starts a new session.
pub async fn upsert_chart_history(
    conn: &SqlitePool,
    snapshot: &history::ChartHistorySnapshot,
) -> Result<i64, sqlx::Error> {
    let detail = serde_json::to_vec(&snapshot.history.detail)
        .map_err(|error| sqlx::Error::Protocol(format!("serialize chart history: {error}")))?;
    let is_remote = i64::from(snapshot.history.is_remote);

    query_as::<_, (i64,)>(
        "INSERT INTO charging_histories \
         (from_level, end_level, charging_time, timestamp, detail, name, udid, \
          is_remote, adapter_name, session_id, history_kind, point_count, detail_format_version) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'chart', ?, 1) \
         ON CONFLICT(session_id) DO UPDATE SET \
           from_level = excluded.from_level, \
           end_level = excluded.end_level, \
           charging_time = excluded.charging_time, \
           timestamp = excluded.timestamp, \
           detail = excluded.detail, \
           name = excluded.name, \
           udid = excluded.udid, \
           is_remote = excluded.is_remote, \
           adapter_name = excluded.adapter_name, \
           history_kind = 'chart', \
           point_count = excluded.point_count, \
           detail_format_version = 1 \
         RETURNING id",
    )
    .bind(snapshot.history.from_level)
    .bind(snapshot.history.end_level)
    .bind(snapshot.history.duration)
    .bind(snapshot.history.timestamp)
    .bind(detail)
    .bind(&snapshot.history.name)
    .bind(&snapshot.history.udid)
    .bind(is_remote)
    .bind(&snapshot.history.adapter_name)
    .bind(&snapshot.session_id)
    .bind(snapshot.point_count as i64)
    .fetch_one(conn)
    .await
    .map(|row| row.0)
}

pub fn setup_database(app: AppHandle) {
    block_in_place(|| {
        async_runtime::block_on(async move {
            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("Failed to get app data directory");

            let db_path = app_data_dir.join(DEFAULT_DATABASE_NAME);

            if !app_data_dir.exists() {
                fs::create_dir_all(&app_data_dir).expect("Failed to create app data directory");
            }

            // All history reads, writes, and secure purge operations share one
            // connection. This prevents a concurrent history view from holding
            // a read transaction while Delete All checkpoints or vacuums.
            let db = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(db_path)
                        .create_if_missing(true)
                        .pragma("secure_delete", "ON"),
                )
                .await
                .unwrap();

            migrate!().run(&db).await.unwrap();

            match compact_chart_history_details(&db).await {
                Ok(updated) if updated > 0 => {
                    log::info!("compacted {updated} chart history detail blob(s)");
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!("chart history detail compaction failed: {error}");
                }
            }

            // A previous delete-all may have committed its row deletion but
            // failed during WAL/VACUUM cleanup. Retry that cleanup on every
            // launch so the warning cannot become permanently orphaned if the
            // user closes the history page or restarts before retrying.
            if let Err(error) = retry_history_cleanup(&db).await {
                log::warn!("startup history cleanup retry failed: {error}");
            }

            app.manage(db);
        });
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use sqlx::{
        migrate, query, query_as, query_scalar,
        sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
        SqlitePool,
    };
    use tempfile::tempdir;

    use crate::history::{
        ChargingHistory, ChargingHistoryDetail, ChartHistorySnapshot, HistoryCurvePoint,
        HistorySummaryData,
    };

    use super::{
        compact_chart_history_details, delete_history_by_id, finish_history_cleanup,
        get_all_charging_history, get_detail_by_id, purge_all_charging_history,
        purge_all_charging_history_with_cleanup, retry_history_cleanup,
        retry_history_cleanup_with_statements, upsert_chart_history,
    };

    fn chart_snapshot(session_id: &str, point_count: usize) -> ChartHistorySnapshot {
        let curve = (0..point_count)
            .map(|index| HistoryCurvePoint {
                is_local: true,
                is_charging: true,
                time_remain: std::time::Duration::from_secs(3600),
                last_update: index as i64,
                adapter_name: Some("Test Adapter".to_string()),
                cycle_count: 25,
                current_capacity: 3500 + index as i32,
                max_capacity: 5000,
                design_capacity: 5200,
                brightness_power_available: true,
                heatpipe_power_available: true,
                data: HistorySummaryData {
                    system_in: index as f32 + 10.0,
                    system_load: index as f32,
                    battery_power: index as f32 + 5.0,
                    brightness_power: 2.5,
                    heatpipe_power: 3.5,
                    temperature: 31.0 + index as f32,
                    ..Default::default()
                },
            })
            .collect::<Vec<_>>();
        ChartHistorySnapshot {
            session_id: session_id.to_string(),
            point_count,
            history: ChargingHistory {
                is_remote: false,
                name: "Test Mac".to_string(),
                udid: "local".to_string(),
                from_level: 50,
                end_level: 50 + point_count.saturating_sub(1) as i32,
                duration: point_count.saturating_sub(1) as i64,
                timestamp: 123,
                adapter_name: "Test Adapter".to_string(),
                detail: ChargingHistoryDetail {
                    avg: HistorySummaryData {
                        system_load: 2.0,
                        temperature: 31.0,
                        ..Default::default()
                    },
                    peak: HistorySummaryData {
                        system_load: 4.0,
                        temperature: 36.0,
                        adapter_power: 45.0,
                        adapter_watts: 67.0,
                        adapter_voltage: 20.0,
                        adapter_amperage: 3.35,
                        ..Default::default()
                    },
                    curve,
                },
            },
        }
    }

    fn assert_marker_absent(db_path: &std::path::Path, marker: &[u8]) {
        for path in [
            db_path.to_path_buf(),
            db_path.with_extension("sqlite-wal"),
            db_path.with_extension("sqlite-shm"),
            db_path.with_extension("sqlite-journal"),
        ] {
            if !path.exists() {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            assert!(
                !bytes.windows(marker.len()).any(|window| window == marker),
                "history marker remained in {}",
                path.display()
            );
        }
    }

    fn assert_marker_present(db_path: &std::path::Path, marker: &[u8]) {
        let present = [
            db_path.to_path_buf(),
            db_path.with_extension("sqlite-wal"),
            db_path.with_extension("sqlite-shm"),
            db_path.with_extension("sqlite-journal"),
        ]
        .into_iter()
        .filter(|path| path.exists())
        .any(|path| {
            fs::read(path)
                .unwrap()
                .windows(marker.len())
                .any(|window| window == marker)
        });
        assert!(present, "test marker was not persisted before compaction");
    }

    fn assert_no_sidecars(db_path: &std::path::Path) {
        for path in [
            db_path.with_extension("sqlite-wal"),
            db_path.with_extension("sqlite-shm"),
            db_path.with_extension("sqlite-journal"),
        ] {
            assert!(
                !path.exists(),
                "database sidecar remained after close: {}",
                path.display()
            );
        }
    }

    #[tokio::test]
    async fn purge_all_history_removes_rows_sequence_and_free_pages() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();

        for timestamp in [1_i64, 2_i64] {
            query(
                "INSERT INTO charging_histories \
                 (from_level, end_level, charging_time, timestamp, detail, name, udid, is_remote, adapter_name) \
                 VALUES (10, 20, 60, ?, ?, 'Test Mac', 'local', 0, 'Test Adapter')",
            )
            .bind(timestamp)
            .bind(vec![0x41_u8; 4096])
            .execute(&pool)
            .await
            .unwrap();
        }

        let result = purge_all_charging_history(&pool).await.unwrap();
        assert_eq!(result.rows_deleted, 2);
        assert_eq!(result.cleanup_error, None);

        let count: i64 = query_scalar("SELECT COUNT(*) FROM charging_histories")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);

        let sequence_count: i64 =
            query_scalar("SELECT COUNT(*) FROM sqlite_sequence WHERE name = 'charging_histories'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sequence_count, 0);

        let freelist_count: i64 = query_scalar("PRAGMA freelist_count")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(freelist_count, 0);
    }

    #[tokio::test]
    async fn purge_reports_cleanup_failure_after_rows_were_committed_deleted() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();
        query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, is_remote, adapter_name) \
             VALUES (10, 20, 60, 1, x'7b7d', 'Test Mac', 'local', 0, 'Test Adapter')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result = purge_all_charging_history_with_cleanup(
            &pool,
            "THIS IS NOT A CHECKPOINT",
            "THIS IS NOT A VACUUM",
        )
        .await
        .unwrap();
        assert_eq!(result.rows_deleted, 1);
        let cleanup_error = result.cleanup_error.unwrap();
        assert!(cleanup_error.contains("WAL checkpoint before VACUUM failed"));
        assert!(cleanup_error.contains("VACUUM failed"));
        assert!(cleanup_error.contains("WAL checkpoint after VACUUM failed"));

        let count: i64 = query_scalar("SELECT COUNT(*) FROM charging_histories")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn chart_session_upsert_reuses_one_row_and_old_rows_remain_readable() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();

        query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, is_remote, adapter_name) \
             VALUES (10, 20, 60, 1, x'7b7d', 'Old Mac', 'local', 0, 'Old Adapter')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let first_id = upsert_chart_history(&pool, &chart_snapshot("session-a", 1))
            .await
            .unwrap();
        let updated_id = upsert_chart_history(&pool, &chart_snapshot("session-a", 3))
            .await
            .unwrap();
        assert_eq!(first_id, updated_id);

        let rows = get_all_charging_history(&pool).await.unwrap();
        assert_eq!(rows.len(), 2);
        let chart = rows.iter().find(|row| row.history_kind == "chart").unwrap();
        assert_eq!(chart.id, first_id);
        assert_eq!(chart.point_count, 3);
        assert!(rows
            .iter()
            .any(|row| row.history_kind == "charging" && row.point_count == 0));

        let (detail, format_version): (Vec<u8>, i64) = query_as(
            "SELECT detail, detail_format_version FROM charging_histories \
             WHERE session_id = 'session-a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(format_version, 1);
        let stored = String::from_utf8(detail.clone()).unwrap();
        assert!(!stored.contains("raw"));
        assert!(!stored.contains("batteryLevel"));
        assert!(!stored.contains("absoluteBatteryLevel"));
        assert!(stored.contains("currentCapacity"));
        assert!(stored.contains("designCapacity"));
        assert!(stored.contains("brightnessPower"));
        assert!(stored.contains("temperature"));
        let detail: ChargingHistoryDetail = serde_json::from_slice(&detail).unwrap();
        assert_eq!(detail.curve.len(), 3);
    }

    #[tokio::test]
    async fn deleted_chart_can_be_saved_again_only_under_a_new_session() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();

        let deleted_id = upsert_chart_history(&pool, &chart_snapshot("session-a", 1))
            .await
            .unwrap();
        assert_eq!(
            delete_history_by_id(&pool, deleted_id)
                .await
                .unwrap()
                .rows_affected(),
            1
        );

        let replacement_id = upsert_chart_history(&pool, &chart_snapshot("session-b", 1))
            .await
            .unwrap();
        assert_ne!(replacement_id, deleted_id);

        let session_ids: Vec<String> =
            query_scalar("SELECT session_id FROM charging_histories ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(session_ids, vec!["session-b"]);
    }

    #[tokio::test]
    async fn cleanup_retry_preserves_history_created_after_delete_all() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();
        let snapshot = chart_snapshot("new-history-after-delete", 3);
        let history_id = upsert_chart_history(&pool, &snapshot).await.unwrap();

        let first_error = retry_history_cleanup_with_statements(
            &pool,
            "THIS IS NOT A CHECKPOINT",
            "THIS IS NOT A VACUUM",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(first_error.contains("WAL checkpoint before VACUUM failed"));
        assert!(first_error.contains("VACUUM failed"));
        assert!(first_error.contains("WAL checkpoint after VACUUM failed"));

        retry_history_cleanup(&pool).await.unwrap();

        let rows = get_all_charging_history(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, history_id);
        assert_eq!(rows[0].point_count, 3);
        let detail: ChargingHistoryDetail =
            serde_json::from_slice(&get_detail_by_id(&pool, history_id).await.unwrap()).unwrap();
        assert_eq!(detail.curve.len(), 3);
    }

    #[tokio::test]
    async fn legacy_v0_2_2_detail_blob_remains_readable_after_migration() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();
        let payload = include_bytes!("../test-data/legacy_history_detail_v0_2_2.json");
        let result = query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, is_remote, adapter_name) \
             VALUES (61, 62, 60, 1735689600, ?, 'Legacy Mac', 'local', 0, '67W USB-C Power Adapter')",
        )
        .bind(payload.as_slice())
        .execute(&pool)
        .await
        .unwrap();

        let bytes = get_detail_by_id(&pool, result.last_insert_rowid())
            .await
            .unwrap();
        let detail: ChargingHistoryDetail = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(detail.curve.len(), 1);
        assert_eq!(detail.avg.temperature, 34.8);
        assert_eq!(detail.peak.adapter_power, 54.0);
        assert_eq!(detail.curve[0].last_update, 1_735_689_600);
        assert_eq!(detail.curve[0].system_load, 18.25);
    }

    #[tokio::test]
    async fn compaction_skips_rows_missing_required_telemetry_fields() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        migrate!().run(&pool).await.unwrap();
        let legacy = include_bytes!("../test-data/legacy_history_detail_v0_2_2.json");
        let mut malformed: serde_json::Value = serde_json::from_slice(legacy).unwrap();
        malformed["avg"]
            .as_object_mut()
            .unwrap()
            .remove("systemLoad");
        let malformed = serde_json::to_vec(&malformed).unwrap();

        let malformed_id = query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, \
              is_remote, adapter_name, session_id, history_kind, point_count) \
             VALUES (61, 62, 60, 1735689600, ?, 'Malformed Chart', 'local', 0, \
                     '67W USB-C Power Adapter', 'malformed-chart', 'chart', 1)",
        )
        .bind(&malformed)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let valid_id = query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, \
              is_remote, adapter_name, session_id, history_kind, point_count) \
             VALUES (61, 62, 60, 1735689600, ?, 'Valid Chart', 'local', 0, \
                     '67W USB-C Power Adapter', 'valid-chart', 'chart', 1)",
        )
        .bind(legacy.as_slice())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        assert_eq!(compact_chart_history_details(&pool).await.unwrap(), 1);

        let (stored_malformed, malformed_version): (Vec<u8>, i64) =
            query_as("SELECT detail, detail_format_version FROM charging_histories WHERE id = ?")
                .bind(malformed_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored_malformed, malformed);
        assert_eq!(malformed_version, 0);

        let valid_version: i64 =
            query_scalar("SELECT detail_format_version FROM charging_histories WHERE id = ?")
                .bind(valid_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(valid_version, 1);
    }

    #[tokio::test]
    async fn compaction_rewrites_only_chart_details_and_preserves_row_summaries() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("history-compaction.sqlite");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .pragma("secure_delete", "ON"),
            )
            .await
            .unwrap();
        migrate!().run(&pool).await.unwrap();

        let legacy = include_bytes!("../test-data/legacy_history_detail_v0_2_2.json");
        let mut current_full: serde_json::Value = serde_json::from_slice(legacy).unwrap();
        current_full["curve"][0]["designCapacity"] = serde_json::json!(5100);
        current_full["curve"][0]["brightnessPowerAvailable"] = serde_json::json!(true);
        current_full["curve"][0]["heatpipePowerAvailable"] = serde_json::json!(true);
        let marker_fragment = "POWERFLOW_CHART_DETAIL_COMPACTION_MARKER_91E7C3";
        current_full["raw"] = serde_json::json!([marker_fragment.repeat(4096)]);
        let current_full = serde_json::to_vec(&current_full).unwrap();

        let chart_id = query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, \
              is_remote, adapter_name, session_id, history_kind, point_count) \
             VALUES (61, 62, 60, 1735689600, ?, 'Chart Mac', 'local', 0, \
                     '67W USB-C Power Adapter', 'legacy-chart', 'chart', 1)",
        )
        .bind(&current_full)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let charging_id = query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, \
              is_remote, adapter_name) \
             VALUES (20, 80, 3600, 1735689500, ?, 'Old Charging Mac', 'local', 0, \
                     '67W USB-C Power Adapter')",
        )
        .bind(legacy.as_slice())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let checkpoint: (i64, i64, i64) = query_as("PRAGMA wal_checkpoint(TRUNCATE)")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(checkpoint.0, 0);
        assert_marker_present(&db_path, marker_fragment.as_bytes());
        let size_before = fs::metadata(&db_path).unwrap().len();

        assert_eq!(compact_chart_history_details(&pool).await.unwrap(), 1);
        retry_history_cleanup(&pool).await.unwrap();
        let size_after = fs::metadata(&db_path).unwrap().len();
        assert!(
            size_after < size_before,
            "VACUUM did not shrink compacted history DB: {size_before} -> {size_after}"
        );
        assert_marker_absent(&db_path, marker_fragment.as_bytes());

        let (from_level, end_level, detail, version): (i64, i64, Vec<u8>, i64) = query_as(
            "SELECT from_level, end_level, detail, detail_format_version \
             FROM charging_histories WHERE id = ?",
        )
        .bind(chart_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((from_level, end_level), (61, 62));
        assert_eq!(version, 1);
        assert!(detail.len() < current_full.len());
        let compact = String::from_utf8(detail.clone()).unwrap();
        assert!(!compact.contains("raw"));
        assert!(!compact.contains("batteryLevel"));
        assert!(!compact.contains("absoluteBatteryLevel"));
        assert!(compact.contains("currentCapacity"));
        assert!(compact.contains("designCapacity"));
        assert!(compact.contains("brightnessPower"));
        assert!(compact.contains("temperature"));
        let detail: ChargingHistoryDetail = serde_json::from_slice(&detail).unwrap();
        assert_eq!(detail.avg.system_load, 18.25);
        assert_eq!(detail.peak.adapter_amperage, 3.35);
        assert_eq!(detail.curve[0].battery_power, 25.1);
        assert_eq!(detail.curve[0].current_capacity, 3500);
        assert_eq!(detail.curve[0].design_capacity, 5100);
        assert!(detail.curve[0].brightness_power_available);
        assert_eq!(detail.curve[0].brightness_power, 2.2);
        assert_eq!(detail.curve[0].temperature, 34.8);

        let (charging_detail, charging_version): (Vec<u8>, i64) =
            query_as("SELECT detail, detail_format_version FROM charging_histories WHERE id = ?")
                .bind(charging_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(charging_detail, legacy);
        assert_eq!(charging_version, 0);
        let old_charging: ChargingHistoryDetail = serde_json::from_slice(&charging_detail).unwrap();
        assert_eq!(old_charging.curve[0].system_in, 44.5);

        assert_eq!(compact_chart_history_details(&pool).await.unwrap(), 0);
        pool.close().await;
        assert_marker_absent(&db_path, marker_fragment.as_bytes());
        assert_no_sidecars(&db_path);
    }

    #[tokio::test]
    async fn purge_all_history_scrubs_database_and_sidecar_files() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("history.sqlite");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        migrate!().run(&pool).await.unwrap();

        let marker_fragment = b"POWERFLOW_HISTORY_PURGE_MARKER_7E3C91";
        let marker = marker_fragment.repeat(256);
        query(
            "INSERT INTO charging_histories \
             (from_level, end_level, charging_time, timestamp, detail, name, udid, is_remote, adapter_name) \
             VALUES (10, 90, 3600, 123, ?, 'Marker Mac', 'local', 0, 'Marker Adapter')",
        )
        .bind(&marker)
        .execute(&pool)
        .await
        .unwrap();

        let result = purge_all_charging_history(&pool).await.unwrap();
        assert_eq!(result.cleanup_error, None);
        assert_marker_absent(&db_path, marker_fragment);

        let integrity: String = query_scalar("PRAGMA integrity_check")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");

        pool.close().await;
        assert_marker_absent(&db_path, marker_fragment);
        assert_no_sidecars(&db_path);
    }

    #[tokio::test]
    async fn final_checkpoint_is_attempted_after_vacuum_failure() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let mut conn = pool.acquire().await.unwrap();

        let error = finish_history_cleanup(
            &mut conn,
            "THIS IS NOT A CHECKPOINT",
            "THIS IS NOT A VACUUM",
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("WAL checkpoint before VACUUM failed"));
        assert!(error.contains("VACUUM failed"));
        assert!(error.contains("WAL checkpoint after VACUUM failed"));
    }
}
