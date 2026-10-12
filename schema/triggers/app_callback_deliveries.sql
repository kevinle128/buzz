
CREATE TRIGGER app_callback_delivery_identity_guard
BEFORE UPDATE OF
    community_id, id, app_id, idempotency_key_hash, payload_hash, event_type,
    route_snapshot, status, event_id, failure_code, created_at, completed_at
ON app_callback_deliveries
FOR EACH ROW
EXECUTE FUNCTION prevent_app_callback_delivery_mutation();
