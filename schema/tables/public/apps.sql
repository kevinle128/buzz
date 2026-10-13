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
