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
