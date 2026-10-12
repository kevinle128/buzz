CREATE TABLE workflow_runs (
    community_id        UUID NOT NULL REFERENCES communities(id),
    id                  UUID NOT NULL DEFAULT gen_random_uuid(),
    workflow_id         UUID NOT NULL,
    status              run_status NOT NULL DEFAULT 'pending',
    trigger_event_id    BYTEA,
    current_step        INT NOT NULL DEFAULT 0,
    execution_trace     JSONB NOT NULL DEFAULT '[]',
    trigger_context     JSONB,
    started_at          TIMESTAMPTZ,
    completed_at        TIMESTAMPTZ,
    error_message       TEXT,
    error_code          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    idempotency_key_hash BYTEA,
    payload_hash        BYTEA,
    route_snapshot      JSONB,
    PRIMARY KEY (community_id, id),
    FOREIGN KEY (community_id, workflow_id)
        REFERENCES workflows (community_id, id) ON DELETE CASCADE,
    CONSTRAINT workflow_runs_idempotency_key_hash_len
        CHECK (idempotency_key_hash IS NULL OR octet_length(idempotency_key_hash) = 32),
    CONSTRAINT workflow_runs_payload_hash_len
        CHECK (payload_hash IS NULL OR octet_length(payload_hash) = 32),
    CONSTRAINT workflow_runs_hash_pair
        CHECK ((idempotency_key_hash IS NULL) = (payload_hash IS NULL)),
    CONSTRAINT workflow_runs_route_requires_hashes
        CHECK (route_snapshot IS NULL OR idempotency_key_hash IS NOT NULL)
);

CREATE INDEX idx_workflow_runs_workflow ON workflow_runs (community_id, workflow_id);
