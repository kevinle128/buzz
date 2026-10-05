-- Immutable webhook route snapshot and control-plane idempotency hashes.
-- Raw idempotency keys are never stored; NULL hashes keep non-dynamic runs unrestricted.
SET LOCAL lock_timeout = '5s';

ALTER TABLE workflow_runs
    ADD COLUMN idempotency_key_hash BYTEA,
    ADD COLUMN payload_hash BYTEA,
    ADD COLUMN route_snapshot JSONB;

ALTER TABLE workflow_runs
    ADD CONSTRAINT workflow_runs_idempotency_key_hash_len
        CHECK (idempotency_key_hash IS NULL OR octet_length(idempotency_key_hash) = 32),
    ADD CONSTRAINT workflow_runs_payload_hash_len
        CHECK (payload_hash IS NULL OR octet_length(payload_hash) = 32),
    ADD CONSTRAINT workflow_runs_hash_pair
        CHECK ((idempotency_key_hash IS NULL) = (payload_hash IS NULL)),
    ADD CONSTRAINT workflow_runs_route_requires_hashes
        CHECK (route_snapshot IS NULL OR idempotency_key_hash IS NOT NULL);

CREATE UNIQUE INDEX idx_workflow_runs_idempotency
    ON workflow_runs (community_id, workflow_id, idempotency_key_hash)
    WHERE idempotency_key_hash IS NOT NULL;

CREATE FUNCTION prevent_workflow_run_admission_identity_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.idempotency_key_hash IS DISTINCT FROM OLD.idempotency_key_hash
       OR NEW.payload_hash IS DISTINCT FROM OLD.payload_hash
       OR NEW.route_snapshot IS DISTINCT FROM OLD.route_snapshot
    THEN
        RAISE EXCEPTION 'workflow run admission identity is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER workflow_run_admission_identity_guard
BEFORE UPDATE OF idempotency_key_hash, payload_hash, route_snapshot ON workflow_runs
FOR EACH ROW
EXECUTE FUNCTION prevent_workflow_run_admission_identity_update();
