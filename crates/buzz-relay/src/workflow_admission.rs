//! Serialized webhook admission for project-channel routing.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use buzz_core::kind::{KIND_GIT_REPO_ANNOUNCEMENT, KIND_PROJECT};
use buzz_core::CommunityId;
use buzz_db::workflow::{RunStatus, WorkflowRunFailure, WorkflowRunRecord};
use buzz_db::workflow_admission::{
    BeginWorkflowAdmission, WorkflowAdmissionGuard, WorkflowRouteSnapshot,
};
use buzz_workflow::executor::TriggerContext;
use buzz_workflow::routing::{
    canonical_payload_hash, hash_idempotency_key, parse_idempotency_key, parse_repository_name,
    strip_idempotency_key, RoutingDef,
};
use buzz_workflow::{RouteFailure, TriggerDef, WorkflowDef};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::api::{api_error, api_error_with_code, internal_error, not_found};
use crate::project_route::{
    authorize_unique_project_route, project_head_from_event, repository_head_from_event,
    resolve_repository_identity, ProjectHead, RepositoryHead,
};
use crate::state::AppState;

enum ResolveRouteError {
    Deterministic(RouteFailure),
    Transient(buzz_db::DbError),
}

#[derive(Serialize)]
struct AdmissionObservation {
    run_id: Option<Uuid>,
    workflow_id: Option<Uuid>,
    community_id: Option<Uuid>,
    outcome: &'static str,
    code: &'static str,
}

impl AdmissionObservation {
    fn for_run(run: &WorkflowRunRecord, outcome: &'static str, code: &'static str) -> Self {
        Self {
            run_id: Some(run.id),
            workflow_id: Some(run.workflow_id),
            community_id: Some(*run.community_id.as_uuid()),
            outcome,
            code,
        }
    }

    fn without_run(
        workflow_id: Option<Uuid>,
        community_id: Option<Uuid>,
        outcome: &'static str,
        code: &'static str,
    ) -> Self {
        Self {
            run_id: None,
            workflow_id,
            community_id,
            outcome,
            code,
        }
    }
}

fn record_admission_observation(observation: AdmissionObservation) {
    metrics::counter!(
        "buzz_workflow_webhook_admission_total",
        "outcome" => observation.outcome,
        "code" => observation.code
    )
    .increment(1);
    tracing::info!(
        run_id = ?observation.run_id,
        workflow_id = ?observation.workflow_id,
        community_id = ?observation.community_id,
        outcome = observation.outcome,
        code = observation.code,
        "workflow webhook admission"
    );
}

fn route_failure_response(failure: RouteFailure) -> (StatusCode, Json<Value>) {
    let status = match failure.http_status() {
        409 => StatusCode::CONFLICT,
        _ => StatusCode::UNPROCESSABLE_ENTITY,
    };
    api_error_with_code(status, failure.redacted_message(), failure.code())
}

fn webhook_fields_from_body(body: Value) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    if let Value::Object(map) = body {
        for (k, v) in map {
            let val_str = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            fields.insert(k, val_str);
        }
    }
    fields
}

fn accepted_run_response(workflow_id: Uuid, run: &WorkflowRunRecord) -> (StatusCode, Json<Value>) {
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "run_id": run.id.to_string(),
            "workflow_id": workflow_id.to_string(),
            "status": run.status.to_string(),
        })),
    )
}

fn replayed_admission_failure(
    run: &WorkflowRunRecord,
) -> Option<(RouteFailure, (StatusCode, Json<Value>))> {
    if run.status != RunStatus::Failed {
        return None;
    }
    let code = run.error_code.as_deref()?;
    let failure = RouteFailure::from_code(code)?;
    if !failure.is_admission_rejection() {
        return None;
    }
    Some((failure, route_failure_response(failure)))
}

async fn resolve_dynamic_route(
    guard: &mut WorkflowAdmissionGuard,
    community_id: CommunityId,
    repository_name: &str,
    routing: &RoutingDef,
    workflow_owner: &[u8],
) -> Result<WorkflowRouteSnapshot, ResolveRouteError> {
    let repository_events = guard
        .list_latest_parameterized_heads(KIND_GIT_REPO_ANNOUNCEMENT as i32)
        .await
        .map_err(ResolveRouteError::Transient)?;
    let repositories: Vec<RepositoryHead> = repository_events
        .iter()
        .filter_map(|stored| repository_head_from_event(&stored.event))
        .collect();
    let (repository_coordinate, tier) =
        resolve_repository_identity(repository_name, &routing.aliases, &repositories)
            .map_err(ResolveRouteError::Deterministic)?;
    let repository = repositories
        .iter()
        .find(|head| head.coordinate == repository_coordinate)
        .ok_or(ResolveRouteError::Deterministic(
            RouteFailure::RepositoryMissing,
        ))?;

    let project_events = guard
        .list_latest_parameterized_heads(KIND_PROJECT as i32)
        .await
        .map_err(ResolveRouteError::Transient)?;
    let projects: Vec<ProjectHead> = project_events
        .iter()
        .filter_map(|stored| project_head_from_event(&stored.event))
        .collect();
    let project = authorize_unique_project_route(repository, &projects)
        .map_err(ResolveRouteError::Deterministic)?;
    let channel_id = project
        .buzz_channel
        .as_deref()
        .ok_or(ResolveRouteError::Deterministic(
            RouteFailure::ProjectChannelInvalid,
        ))?
        .parse::<Uuid>()
        .map_err(|_| ResolveRouteError::Deterministic(RouteFailure::ProjectChannelInvalid))?;
    let channel = guard
        .load_destination_channel(channel_id)
        .await
        .map_err(ResolveRouteError::Transient)?
        .ok_or(ResolveRouteError::Deterministic(
            RouteFailure::ProjectChannelInvalid,
        ))?;
    if channel.archived_at.is_some() {
        return Err(ResolveRouteError::Deterministic(
            RouteFailure::ProjectChannelInvalid,
        ));
    }
    let role = guard
        .destination_member_role(channel_id, workflow_owner)
        .await
        .map_err(ResolveRouteError::Transient)?;
    if role.is_none() {
        return Err(ResolveRouteError::Deterministic(
            RouteFailure::RouteUnauthorized,
        ));
    }

    Ok(WorkflowRouteSnapshot {
        community_id: *community_id.as_uuid(),
        repository_coordinate,
        project_coordinate: project.coordinate.clone(),
        channel_id,
        matched_identity_tier: tier.as_str().to_owned(),
    })
}

fn spawn_admitted_run(
    state: &Arc<AppState>,
    community_id: CommunityId,
    run_id: Uuid,
    definition: Value,
    trigger_ctx: TriggerContext,
) {
    let engine = Arc::clone(&state.workflow_engine);
    let db = state.db.clone();
    tokio::spawn(async move {
        let def: WorkflowDef = match serde_json::from_value(definition) {
            Ok(d) => d,
            Err(e) => {
                let _ = db
                    .update_workflow_run(
                        community_id,
                        run_id,
                        RunStatus::Failed,
                        0,
                        &serde_json::json!([]),
                        Some(WorkflowRunFailure {
                            code: "invalid_definition",
                            message: &format!("definition parse error: {e}"),
                        }),
                    )
                    .await;
                return;
            }
        };

        let result = buzz_workflow::executor::execute_from_step(
            &engine,
            community_id,
            run_id,
            &def,
            &trigger_ctx,
            0,
            None,
        )
        .await;
        engine
            .finalize_run(community_id, run_id, result, None)
            .await;
    });
}

/// Handle a webhook trigger after the bridge extracts its route and query data.
pub(crate) async fn handle_workflow_webhook(
    state: Arc<AppState>,
    id_str: String,
    query_secret: Option<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let id = Uuid::parse_str(&id_str)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid workflow UUID"))?;

    // Row zero: bind this webhook to its community from the request host before
    // any tenant-scoped lookup or write. The host — not the workflow row —
    // determines the tenant: a request for community A's host may only reach
    // community A's workflows, even when the same workflow UUID also exists in
    // community B. Unmapped host, lookup failure, and a workflow that does not
    // exist in *this* community all fail closed with the same generic 404, so a
    // caller cannot probe which hosts or workflow ids exist on other tenants.
    let raw_host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let tenant = crate::tenant::bind_community(&state.db, raw_host)
        .await
        .map_err(|_| not_found("workflow not found"))?;
    let community_id = tenant.community();

    let workflow = state
        .db
        .get_workflow(community_id, id)
        .await
        .map_err(|_| not_found("workflow not found"))?;

    // Verify webhook secret. Prefer header (not logged by proxies); fall back to query param.
    // Until the caller proves the secret, every failure answers like a missing
    // workflow, so the response never reveals that the workflow exists, its
    // trigger type, or whether a secret was configured.
    let stored_secret = crate::webhook_secret::extract_secret(&workflow.definition);
    let provided_secret = headers
        .get("x-webhook-secret")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or(query_secret)
        .unwrap_or_default();
    if !stored_secret
        .is_some_and(|secret| crate::webhook_secret::verify_secret(&provided_secret, &secret))
    {
        record_admission_observation(AdmissionObservation::without_run(
            Some(id),
            Some(*community_id.as_uuid()),
            "rejected",
            "none",
        ));
        return Err(not_found("workflow not found"));
    }

    let def: WorkflowDef = serde_json::from_value(workflow.definition.clone())
        .map_err(|e| internal_error(&format!("corrupt workflow definition: {e}")))?;

    if !matches!(def.trigger, TriggerDef::Webhook) {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "workflow does not have a webhook trigger",
        ));
    }

    if def.has_project_channel_routing() {
        return handle_dynamic_webhook(state, community_id, id, workflow, def, body).await;
    }

    handle_static_webhook(state, community_id, id, workflow, def, body).await
}

async fn handle_static_webhook(
    state: Arc<AppState>,
    community_id: CommunityId,
    id: Uuid,
    workflow: buzz_db::workflow::WorkflowRecord,
    def: WorkflowDef,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    // Parse optional JSON body as trigger context.
    let body_json: Option<Value> =
        if body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&body).map_err(|e| {
                api_error(StatusCode::BAD_REQUEST, &format!("invalid JSON body: {e}"))
            })?)
        };

    // Build trigger context from webhook body fields.
    let mut trigger_ctx = TriggerContext {
        channel_id: workflow
            .channel_id
            .map(|ch| ch.to_string())
            .unwrap_or_default(),
        ..Default::default()
    };
    if let Some(Value::Object(ref map)) = body_json {
        for (k, v) in map {
            let val_str = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            trigger_ctx.webhook_fields.insert(k.clone(), val_str);
        }
    }
    let trigger_ctx_json = serde_json::to_value(&trigger_ctx).ok();

    // SEC-006: the webhook secret authenticates the *caller*, but the run
    // executes with the workflow **owner's** standing authority — so the
    // secret alone is insufficient. Immediately before run creation, reject
    // disabled/inactive workflows and recheck the owner's current channel
    // membership (and role, for exfiltration-capable definitions). Fail
    // closed with the same generic 404 as the lookups above so a
    // revoked-owner workflow is indistinguishable from a nonexistent one.
    if !workflow.enabled || workflow.status != buzz_db::workflow::WorkflowStatus::Active {
        return Err(not_found("workflow not found"));
    }
    let Some(wf_channel_id) = workflow.channel_id else {
        // No channel scope means no channel authority to verify — fail closed.
        return Err(not_found("workflow not found"));
    };
    state
        .workflow_engine
        .check_owner_authority(community_id, wf_channel_id, &workflow.owner_pubkey, &def)
        .await
        .map_err(|_| not_found("workflow not found"))?;

    let run_id = state
        .db
        .create_workflow_run(community_id, id, None, trigger_ctx_json.as_ref())
        .await
        .map_err(|e| internal_error(&format!("db error: {e}")))?;

    spawn_admitted_run(
        &state,
        community_id,
        run_id,
        workflow.definition.clone(),
        trigger_ctx,
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "run_id": run_id.to_string(),
            "workflow_id": id.to_string(),
            "status": "pending",
        })),
    ))
}

async fn handle_dynamic_webhook(
    state: Arc<AppState>,
    community_id: CommunityId,
    id: Uuid,
    workflow: buzz_db::workflow::WorkflowRecord,
    def: WorkflowDef,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    if !workflow.enabled || workflow.status != buzz_db::workflow::WorkflowStatus::Active {
        return Err(not_found("workflow not found"));
    }
    let Some(wf_channel_id) = workflow.channel_id else {
        return Err(not_found("workflow not found"));
    };
    state
        .workflow_engine
        .check_owner_authority(community_id, wf_channel_id, &workflow.owner_pubkey, &def)
        .await
        .map_err(|_| not_found("workflow not found"))?;

    let Some(routing) = def.routing.as_ref() else {
        return Err(not_found("workflow not found"));
    };
    let body_value: Value = serde_json::from_slice(&body)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "invalid JSON body"))?;
    if !body_value.is_object() {
        let failure = RouteFailure::InvalidControlFields;
        record_admission_observation(AdmissionObservation::without_run(
            Some(id),
            Some(*community_id.as_uuid()),
            "rejected",
            failure.code(),
        ));
        return Err(api_error_with_code(
            StatusCode::UNPROCESSABLE_ENTITY,
            failure.redacted_message(),
            failure.code(),
        ));
    }
    let raw_key = match parse_idempotency_key(&body_value) {
        Ok(raw_key) => raw_key,
        Err(failure) => {
            record_admission_observation(AdmissionObservation::without_run(
                Some(id),
                Some(*community_id.as_uuid()),
                "rejected",
                failure.code(),
            ));
            return Err(route_failure_response(failure));
        }
    };
    let sanitized_body = strip_idempotency_key(body_value);
    let key_hash = hash_idempotency_key(&raw_key);
    drop(raw_key);
    let payload_hash = canonical_payload_hash(&sanitized_body)
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "service unavailable"))?;
    let trigger_ctx = TriggerContext {
        channel_id: workflow
            .channel_id
            .map(|channel| channel.to_string())
            .unwrap_or_default(),
        webhook_fields: webhook_fields_from_body(sanitized_body.clone()),
        ..Default::default()
    };
    let trigger_ctx_json = serde_json::to_value(&trigger_ctx)
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "service unavailable"))?;

    let begin = match state
        .db
        .begin_workflow_admission(community_id, id, &key_hash, &payload_hash)
        .await
    {
        Ok(begin) => begin,
        Err(_) => {
            record_admission_observation(AdmissionObservation::without_run(
                Some(id),
                Some(*community_id.as_uuid()),
                "unavailable",
                "none",
            ));
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "service unavailable",
            ));
        }
    };
    let mut guard = match begin {
        BeginWorkflowAdmission::Existing(run) => {
            if let Some((failure, response)) = replayed_admission_failure(&run) {
                record_admission_observation(AdmissionObservation::for_run(
                    &run,
                    "rejected",
                    failure.code(),
                ));
                return Err(response);
            }
            record_admission_observation(AdmissionObservation::for_run(&run, "accepted", "none"));
            return Ok(accepted_run_response(id, &run));
        }
        BeginWorkflowAdmission::PayloadConflict { existing } => {
            let failure = RouteFailure::IdempotencyConflict;
            record_admission_observation(AdmissionObservation::for_run(
                &existing,
                "conflict",
                failure.code(),
            ));
            return Err(api_error_with_code(
                StatusCode::CONFLICT,
                failure.redacted_message(),
                failure.code(),
            ));
        }
        BeginWorkflowAdmission::Vacant(guard) => guard,
    };

    let repository_name = match parse_repository_name(&sanitized_body) {
        Ok(name) => name,
        Err(failure) => {
            let run = guard
                .reject(
                    &trigger_ctx_json,
                    WorkflowRunFailure {
                        code: failure.code(),
                        message: failure.redacted_message(),
                    },
                )
                .await
                .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "service unavailable"))?;
            record_admission_observation(AdmissionObservation::for_run(
                &run,
                "rejected",
                failure.code(),
            ));
            return Err(route_failure_response(failure));
        }
    };

    let snapshot = match resolve_dynamic_route(
        &mut guard,
        community_id,
        &repository_name,
        routing,
        &workflow.owner_pubkey,
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(ResolveRouteError::Deterministic(failure)) => {
            let run = guard
                .reject(
                    &trigger_ctx_json,
                    WorkflowRunFailure {
                        code: failure.code(),
                        message: failure.redacted_message(),
                    },
                )
                .await
                .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "service unavailable"))?;
            record_admission_observation(AdmissionObservation::for_run(
                &run,
                "rejected",
                failure.code(),
            ));
            return Err(route_failure_response(failure));
        }
        Err(ResolveRouteError::Transient(_error)) => {
            drop(guard);
            record_admission_observation(AdmissionObservation::without_run(
                Some(id),
                Some(*community_id.as_uuid()),
                "unavailable",
                "none",
            ));
            return Err(api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "service unavailable",
            ));
        }
    };
    let run = guard
        .accept(&trigger_ctx_json, &snapshot)
        .await
        .map_err(|_| api_error(StatusCode::SERVICE_UNAVAILABLE, "service unavailable"))?;
    record_admission_observation(AdmissionObservation::for_run(&run, "accepted", "none"));
    spawn_admitted_run(
        &state,
        community_id,
        run.id,
        workflow.definition.clone(),
        trigger_ctx,
    );
    Ok(accepted_run_response(id, &run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::BTreeSet;

    fn sample_run(status: RunStatus, error_code: Option<&str>) -> WorkflowRunRecord {
        WorkflowRunRecord {
            id: Uuid::new_v4(),
            community_id: CommunityId::from_uuid(Uuid::new_v4()),
            workflow_id: Uuid::new_v4(),
            status,
            trigger_event_id: None,
            current_step: 0,
            execution_trace: serde_json::json!([]),
            trigger_context: None,
            started_at: None,
            completed_at: None,
            error_message: error_code.map(|_| "redacted".to_string()),
            error_code: error_code.map(str::to_string),
            created_at: Utc::now(),
            idempotency_key_hash: None,
            payload_hash: None,
            route_snapshot: None,
        }
    }

    #[test]
    fn route_failure_conflict_is_409() {
        let (status, Json(body)) = route_failure_response(RouteFailure::IdempotencyConflict);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "idempotency_conflict");
        assert!(body.get("run_id").is_none());
    }

    #[test]
    fn deterministic_route_failure_is_redacted_422_without_run_id() {
        let (status, Json(body)) = route_failure_response(RouteFailure::RepositoryMissing);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "repository_missing");
        assert_eq!(body["error"], "repository could not be resolved");
        assert!(body.get("run_id").is_none());
    }

    #[test]
    fn admission_strips_key_before_trigger_context() {
        let body = serde_json::json!({
            "repository_name": "agentic-os-plan",
            "idempotency_key": "raw-secret",
            "title": "hello"
        });
        let fields = webhook_fields_from_body(strip_idempotency_key(body.clone()));
        assert!(!fields.contains_key("idempotency_key"));
        assert_eq!(
            fields.get("repository_name").map(String::as_str),
            Some("agentic-os-plan")
        );
        assert_eq!(fields.get("title").map(String::as_str), Some("hello"));
    }

    #[test]
    fn accepted_run_response_emits_only_run_id_workflow_id_and_status() {
        let run = sample_run(RunStatus::Running, None);
        let workflow_id = Uuid::new_v4();
        let (status, Json(body)) = accepted_run_response(workflow_id, &run);
        assert_eq!(status, StatusCode::ACCEPTED);
        let obj = body.as_object().expect("object body");
        assert_eq!(obj.len(), 3);
        assert_eq!(body["run_id"], run.id.to_string());
        assert_eq!(body["workflow_id"], workflow_id.to_string());
        assert_eq!(body["status"], "running");
        assert!(body.get("code").is_none());
        assert!(body.get("error").is_none());
        assert!(body.get("error_code").is_none());
        assert!(body.get("route_snapshot").is_none());
    }

    #[test]
    fn replayed_admission_failure_replays_repository_missing() {
        let run = sample_run(RunStatus::Failed, Some("repository_missing"));
        let (failure, (status, Json(body))) =
            replayed_admission_failure(&run).expect("admission rejection");
        assert_eq!(failure, RouteFailure::RepositoryMissing);
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["code"], "repository_missing");
        assert_eq!(body["error"], "repository could not be resolved");
        assert!(body.get("run_id").is_none());
    }

    #[test]
    fn replayed_admission_failure_ignores_already_admitted_runs() {
        let stale = sample_run(RunStatus::Failed, Some("route_stale"));
        assert!(replayed_admission_failure(&stale).is_none());
        let execution = sample_run(RunStatus::Failed, Some("invalid_definition"));
        assert!(replayed_admission_failure(&execution).is_none());
        let completed = sample_run(RunStatus::Completed, Some("repository_missing"));
        assert!(replayed_admission_failure(&completed).is_none());
    }

    #[test]
    fn admission_observation_contains_only_safe_low_cardinality_fields() {
        let run = sample_run(RunStatus::Failed, Some("repository_missing"));
        let observation = AdmissionObservation::for_run(&run, "rejected", "repository_missing");
        let value = serde_json::to_value(&observation).expect("serialize observation");
        let obj = value.as_object().expect("object");
        let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            BTreeSet::from(["run_id", "workflow_id", "community_id", "outcome", "code"])
        );
        assert_eq!(obj["run_id"], run.id.to_string());
        assert_eq!(obj["workflow_id"], run.workflow_id.to_string());
        assert_eq!(obj["community_id"], run.community_id.as_uuid().to_string());
        assert_eq!(obj["outcome"], "rejected");
        assert_eq!(obj["code"], "repository_missing");
        let serialized = serde_json::to_string(&observation).expect("string");
        for forbidden in [
            "secret",
            "idempotency_key",
            "raw-secret",
            "callback",
            "candidate",
            "repository_name",
            "30617:",
            "30621:",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "observation leaked {forbidden}: {serialized}"
            );
        }

        let without = AdmissionObservation::without_run(
            Some(run.workflow_id),
            Some(*run.community_id.as_uuid()),
            "unavailable",
            "none",
        );
        let without_value = serde_json::to_value(&without).expect("serialize without_run");
        assert!(without_value["run_id"].is_null());
        assert_eq!(without_value["outcome"], "unavailable");
        assert_eq!(without_value["code"], "none");
    }
}
