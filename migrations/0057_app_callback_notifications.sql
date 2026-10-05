-- Community Apps and the immutable callback delivery ledger.
-- Raw callback secrets and raw idempotency keys are never stored.
SET LOCAL lock_timeout = '5s';

CREATE TABLE apps (
    community_id    UUID NOT NULL REFERENCES communities(id),
    id              UUID NOT NULL,
    name            TEXT NOT NULL,
    description     TEXT,
    icon_url        TEXT,
    status          TEXT NOT NULL DEFAULT 'active',
    secret_hash     BYTEA NOT NULL,
    created_by      BYTEA NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (community_id, id),
    CONSTRAINT chk_apps_id_not_nil
        CHECK (id <> '00000000-0000-0000-0000-000000000000'::uuid),
    CONSTRAINT chk_apps_name_len
        CHECK (char_length(name) BETWEEN 1 AND 128),
    CONSTRAINT chk_apps_description_len
        CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 2048),
    CONSTRAINT chk_apps_icon_url_len
        CHECK (icon_url IS NULL OR octet_length(icon_url) BETWEEN 1 AND 4096),
    CONSTRAINT chk_apps_status
        CHECK (status IN ('active', 'disabled')),
    CONSTRAINT chk_apps_secret_hash_len
        CHECK (octet_length(secret_hash) = 32),
    CONSTRAINT chk_apps_created_by_len
        CHECK (octet_length(created_by) = 32)
);

CREATE FUNCTION prevent_app_created_by_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.created_by IS DISTINCT FROM OLD.created_by THEN
        RAISE EXCEPTION 'apps.created_by is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.community_id IS DISTINCT FROM OLD.community_id
       OR NEW.id IS DISTINCT FROM OLD.id THEN
        RAISE EXCEPTION 'apps identity is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER apps_created_by_guard
BEFORE UPDATE OF created_by, community_id, id ON apps
FOR EACH ROW
EXECUTE FUNCTION prevent_app_created_by_update();

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

CREATE FUNCTION prevent_app_callback_delivery_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.community_id IS DISTINCT FROM OLD.community_id
       OR NEW.id IS DISTINCT FROM OLD.id
       OR NEW.app_id IS DISTINCT FROM OLD.app_id
       OR NEW.idempotency_key_hash IS DISTINCT FROM OLD.idempotency_key_hash
       OR NEW.payload_hash IS DISTINCT FROM OLD.payload_hash
       OR NEW.event_type IS DISTINCT FROM OLD.event_type
       OR NEW.route_snapshot IS DISTINCT FROM OLD.route_snapshot
       OR NEW.status IS DISTINCT FROM OLD.status
       OR NEW.event_id IS DISTINCT FROM OLD.event_id
       OR NEW.failure_code IS DISTINCT FROM OLD.failure_code
       OR NEW.created_at IS DISTINCT FROM OLD.created_at
       OR NEW.completed_at IS DISTINCT FROM OLD.completed_at
    THEN
        RAISE EXCEPTION 'app callback delivery identity is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER app_callback_delivery_identity_guard
BEFORE UPDATE OF
    community_id, id, app_id, idempotency_key_hash, payload_hash, event_type,
    route_snapshot, status, event_id, failure_code, created_at, completed_at
ON app_callback_deliveries
FOR EACH ROW
EXECUTE FUNCTION prevent_app_callback_delivery_mutation();

SELECT attach_community_write_fence('apps');
SELECT attach_community_write_fence('app_callback_deliveries');
