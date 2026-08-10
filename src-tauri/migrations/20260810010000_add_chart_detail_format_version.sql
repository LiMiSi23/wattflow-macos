ALTER TABLE charging_histories
    ADD COLUMN detail_format_version INTEGER NOT NULL DEFAULT 0;
