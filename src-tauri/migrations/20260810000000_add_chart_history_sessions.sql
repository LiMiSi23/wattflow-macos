ALTER TABLE charging_histories ADD COLUMN session_id TEXT;
ALTER TABLE charging_histories ADD COLUMN history_kind TEXT NOT NULL DEFAULT 'charging';
ALTER TABLE charging_histories ADD COLUMN point_count INTEGER NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX IF NOT EXISTS charging_histories_session_id_unique
    ON charging_histories (session_id);
