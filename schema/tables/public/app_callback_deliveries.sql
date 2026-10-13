CREATE TABLE app_callback_deliveries (
    community_id            UUID NOT NULL REFERENCES communities(id),
    id                      UUID NOT NULL,
    app_id                  UUID NOT NULL,
    idempotency_key_hash    BYTEA NOT NULL,
    payload_hash            BYTEA NOT NULL,
    event_type              TEXT NOT NULL,
    route_snapshot          JSONB,
    status                  TEXT NOT NULL,
    event_id                BYTEA,
    failure_code            TEXT,
    created_at              TIMESTAMPTZ NOT NULL,
    completed_at            TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (community_id, id),
    FOREIGN KEY (community_id, app_id)
        REFERENCES apps (community_id, id),
    CONSTRAINT chk_app_callback_deliveries_id_not_nil
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT chk_app_callback_deliveries_idempotency_key_hash_len
        CHECK (octet_length(idempotency_key_hash) = 32),
    CONSTRAINT chk_app_callback_deliveries_payload_hash_len
        CHECK (octet_length(payload_hash) = 32),
    CONSTRAINT chk_app_callback_deliveries_event_id_len
        CHECK (event_id IS NULL OR octet_length(event_id) = 32),
    CONSTRAINT chk_app_callback_deliveries_event_type
        CHECK (event_type ~ '^[a-z0-9][a-z0-9._:-]{0,63}$'),
    CONSTRAINT chk_app_callback_deliveries_failure_code
        CHECK (failure_code IS NULL OR char_length(failure_code) BETWEEN 1 AND 64),
    CONSTRAINT chk_app_callback_deliveries_status
        CHECK (status IN ('delivered', 'rejected')),
    CONSTRAINT chk_app_callback_deliveries_outcome_shape
        CHECK (
            (status = 'delivered'
                AND event_id IS NOT NULL
                AND route_snapshot IS NOT NULL
                AND failure_code IS NULL)
            OR
            (status = 'rejected'
                AND failure_code IS NOT NULL
                AND event_id IS NULL)
        )
);

CREATE UNIQUE INDEX idx_app_callback_deliveries_idempotency
    ON app_callback_deliveries (community_id, app_id, idempotency_key_hash);
