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
