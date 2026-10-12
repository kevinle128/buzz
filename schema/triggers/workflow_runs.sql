SELECT attach_community_write_fence('workflow_runs');

CREATE TRIGGER workflow_run_admission_identity_guard
BEFORE UPDATE OF idempotency_key_hash, payload_hash, route_snapshot ON workflow_runs
FOR EACH ROW
EXECUTE FUNCTION prevent_workflow_run_admission_identity_update();
