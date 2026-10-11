//! Characterization of the lifecycle controller's refusal outcomes: which
//! prior lifecycles the lifecycle writers in this module and in
//! `wake_completion` refuse, what `action` and `untracked_action` return when
//! they refuse, and what the `sandboxes` row looks like afterwards. The
//! completion CAS itself (`complete_wake_lifecycle`) is swept in
//! `wake_completion_postgres_tests`.
//!
//! Every case runs against the direct substrate. Paths that only the
//! Kubernetes substrate can reach (the redrive's kube hydration probe, the
//! drain-path `reachable_runtime_client == None` refusal) are not yet
//! covered: reaching them needs a fake kube API or an injectable substrate
//! seam.

use axum::http::StatusCode;
use pplx_space_common_core::types::{NodeId, NodeManagedBy, SandboxId};
use pplx_space_db_test_fixtures::pg_test_db_with_max_connections;
use pplx_space_metrics::sandbox::api::{SandboxApiMetricIdentity, WakeCompletionAction};
use pplx_space_node_api_sim::{SandboxRecord, SimState, SimWorkerHarness};
use pplx_space_test_env::TestResult;

use super::*;
use crate::controllers::sandbox::wake_completion::wake_completion_redrive;
use crate::controllers::substrate::Substrate;
use crate::controllers::substrate::direct::DirectSubstrate;

/// Values `sandboxes.lifecycle` can hold that are not `SandboxLifecycle`
/// variants (see `SandboxLifecycle::all`).
const NON_VARIANT_LIFECYCLES: [&str; 2] = ["deleted", "cleanup_required"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    lifecycle: String,
    placement: String,
}

impl Row {
    fn new(lifecycle: &str, placement: &str) -> Self {
        Self {
            lifecycle: lifecycle.to_owned(),
            placement: placement.to_owned(),
        }
    }
}

async fn insert_sandbox(
    pool: &PgPool,
    sandbox_id: SandboxId,
    lifecycle: &str,
    managed_by: &str,
) -> TestResult {
    sqlx::query(sql!(
        "INSERT INTO sandboxes (
             id, user_id, org_id, managed_by, lifecycle, placement,
             template_id, content_hash, network_policy_override_json,
             network_policy_generation
         )
         VALUES (
             $1,
             '00000000-0000-0000-0000-000000000010',
             '00000000-0000-0000-0000-000000000001',
             $2, $3, 'unplaced', 'seed', 'hash-seed', $4, 1
         )"
    ))
    .bind(sandbox_id)
    .bind(managed_by)
    .bind(lifecycle)
    .bind(sqlx::types::Json(
        pplx_space_net_common::NetworkPolicyV1::default(),
    ))
    .execute(pool)
    .await?;
    Ok(())
}

async fn place_on_node(pool: &PgPool, sandbox_id: SandboxId, node_id: NodeId) -> TestResult {
    sqlx::query(sql!(
        "UPDATE sandboxes
            SET placement = 'on_node', active_node_id = $2, auto_lifecycle_json = $3
          WHERE id = $1"
    ))
    .bind(sandbox_id)
    .bind(node_id)
    .bind(sqlx::types::Json(crate::e2b::auto_lifecycle_policy(
        60, true, true,
    )))
    .execute(pool)
    .await?;
    Ok(())
}

async fn set_placement(pool: &PgPool, sandbox_id: SandboxId, placement: &str) -> TestResult {
    sqlx::query(sql!("UPDATE sandboxes SET placement = $2 WHERE id = $1"))
        .bind(sandbox_id)
        .bind(placement)
        .execute(pool)
        .await?;
    Ok(())
}

async fn set_lifecycle(pool: &PgPool, sandbox_id: SandboxId, lifecycle: &str) -> TestResult {
    sqlx::query(sql!("UPDATE sandboxes SET lifecycle = $2 WHERE id = $1"))
        .bind(sandbox_id)
        .bind(lifecycle)
        .execute(pool)
        .await?;
    Ok(())
}

async fn row(pool: &PgPool, sandbox_id: SandboxId) -> TestResult<Option<Row>> {
    Ok(sqlx::query_as::<_, (String, String)>(sql!(
        "SELECT lifecycle, placement FROM sandboxes WHERE id = $1"
    ))
    .bind(sandbox_id)
    .fetch_optional(pool)
    .await?
    .map(|(lifecycle, placement)| Row {
        lifecycle,
        placement,
    }))
}

async fn tracked(pool: &PgPool, sandbox_id: SandboxId) -> TestResult<Sandbox> {
    Ok(crate::controllers::sandbox::fetch_sandbox(pool, sandbox_id)
        .await?
        .expect("sandbox row exists"))
}

fn direct_substrate(pool: &PgPool) -> Substrate {
    Substrate::Direct(Box::new(DirectSubstrate::new(pool.clone())))
}

fn direct_controller(pool: &PgPool) -> SandboxController {
    SandboxController::new(direct_substrate(pool))
}

/// Cluster admission enabled over an empty direct inventory: every charged
/// activation is refused with a 429, so a refusal that is *not* a 429 proves
/// the refusing check ran before admission.
fn controller_with_empty_cluster_admission(pool: &PgPool) -> SandboxController {
    let admission = crate::cluster_admission::ClusterSandboxAdmission::with_config(
        direct_substrate(pool),
        crate::cluster_admission::ClusterSandboxAdmissionConfig {
            enabled: true,
            refill_rps: 6.0,
            burst: 60.0,
            redis_url: None,
        },
        pplx_space_feature_flags::FeatureFlagClient::disabled(),
    );
    SandboxController::with_cluster_admission(direct_substrate(pool), admission)
}

fn identity(action: SandboxAction) -> SandboxApiMetricIdentity {
    SandboxApiMetricIdentity::native(action.as_str())
}

async fn run_action(
    controller: &SandboxController,
    pool: &PgPool,
    sandbox_id: SandboxId,
    sandbox: Option<&Sandbox>,
    action: SandboxAction,
    activation: Option<SandboxActivationRequest>,
) -> Result<serde_json::Value, ControllerError> {
    controller
        .action(
            pool,
            sandbox_id,
            sandbox,
            identity(action),
            action,
            activation,
        )
        .await
}

/// A registered direct worker backed by the in-process simulator. Callers
/// must have entered `enter_sim_worker_process` first.
async fn start_sim_worker(pool: &PgPool) -> TestResult<(SimWorkerHarness, NodeId)> {
    let node_id = NodeId::new();
    let harness = SimWorkerHarness::builder(node_id.to_string())
        .with_internal_api()
        .start()
        .await?;
    sqlx::query(sql!(
        "INSERT INTO nodes (id, address, role, managed_by, state)
         VALUES ($1, '127.0.0.1', 'worker', 'space', 'ready')"
    ))
    .bind(node_id)
    .execute(pool)
    .await?;
    sqlx::query(sql!(
        "INSERT INTO node_direct (node_id, node_api_endpoint) VALUES ($1, $2)"
    ))
    .bind(node_id)
    .bind(harness.endpoint())
    .execute(pool)
    .await?;
    Ok((harness, node_id))
}

async fn insert_resident(state: &SimState, sandbox_id: SandboxId, status: &str) -> TestResult {
    let mut record = SandboxRecord::new_creating(
        sandbox_id.to_string(),
        "seed".to_owned(),
        "fc".to_owned(),
        "root".to_owned(),
        Some("hash-seed".to_owned()),
        false,
    )
    .with_route_identity_provenance(1, 1, Some(NodeManagedBy::Space));
    record.status = status.to_owned();
    state.insert_new(record).await?;
    Ok(())
}

async fn set_drain_phase(pool: &PgPool, node_id: NodeId, phase: &str) -> TestResult {
    sqlx::query(sql!(
        "UPDATE nodes SET drain_phase = $2, drain_mode = 'forced' WHERE id = $1"
    ))
    .bind(node_id)
    .bind(phase)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// `action` refusal arms that return before any substrate call.
// ---------------------------------------------------------------------------

/// Wake (explicit or implicit) of a teardown or terminal lifecycle is a 409
/// naming the lifecycle. It is refused before cluster admission, so it is
/// never a 429 even with an exhausted limiter, and it writes nothing. Every
/// other stored lifecycle passes the entry guard and is refused later (by the
/// empty cluster's admission, or by the substrate for a restore-shaped wake)
/// with something other than a 409.
///
/// The entry guard is deliberately narrower than the completion CAS
/// (`complete_wake_lifecycle`): `suspending` passes here and the CAS defers
/// to it (as it does to `suspended` on Kubernetes); on direct, `suspended` is
/// the restore baseline the CAS advances from.
#[tokio::test(flavor = "current_thread")]
async fn wake_refuses_teardown_and_terminal_lifecycles_before_admission() -> TestResult {
    let db = pg_test_db_with_max_connections(3).await?;
    let pool = &db.pool;
    let controller = controller_with_empty_cluster_admission(pool);

    let variants = SandboxLifecycle::all().iter().map(|lifecycle| {
        let refused = match lifecycle {
            SandboxLifecycle::Stopping
            | SandboxLifecycle::Stopped
            | SandboxLifecycle::Deleting
            | SandboxLifecycle::Quarantined
            | SandboxLifecycle::Failed => true,
            SandboxLifecycle::Created
            | SandboxLifecycle::Creating
            | SandboxLifecycle::Starting
            | SandboxLifecycle::Started
            | SandboxLifecycle::Restoring
            | SandboxLifecycle::Pausing
            | SandboxLifecycle::Paused
            | SandboxLifecycle::Resuming
            | SandboxLifecycle::Checkpointing
            | SandboxLifecycle::Suspending
            | SandboxLifecycle::Suspended => false,
        };
        (lifecycle.as_str(), refused)
    });
    let non_variants = NON_VARIANT_LIFECYCLES.map(|lifecycle| (lifecycle, true));
    for (lifecycle, refused) in variants.chain(non_variants) {
        for action in [
            SandboxAction::Wake,
            SandboxAction::ImplicitWake(ExecutionActivityKind::Access),
        ] {
            let sandbox_id = SandboxId::new();
            insert_sandbox(pool, sandbox_id, lifecycle, "space").await?;
            let sandbox = tracked(pool, sandbox_id).await?;

            let error = run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None)
                .await
                .expect_err("the empty cluster admits no wake");

            if refused {
                assert_eq!(
                    error.status(),
                    StatusCode::CONFLICT,
                    "{action:?} on {lifecycle}"
                );
                assert_eq!(
                    error.to_string(),
                    format!("sandbox cannot be woken while lifecycle is {lifecycle}"),
                );
                assert_eq!(
                    row(pool, sandbox_id).await?,
                    Some(Row::new(lifecycle, "unplaced"))
                );
            } else {
                assert_ne!(
                    error.status(),
                    StatusCode::CONFLICT,
                    "{action:?} on {lifecycle} passes the entry guard: {error}"
                );
            }
        }
    }
    Ok(())
}

/// The internal access-path labels are not lifecycle verbs: `action` answers
/// 400 for them, and `untracked_action` additionally refuses the DB-only
/// snapshot-policy verbs, which need a tracked row to update.
#[tokio::test(flavor = "current_thread")]
async fn non_lifecycle_actions_are_bad_requests_and_write_nothing() -> TestResult {
    let db = pg_test_db_with_max_connections(2).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let internal_labels = [
        SandboxAction::Access,
        SandboxAction::Stats,
        SandboxAction::OpenPort,
        SandboxAction::Rollback,
        SandboxAction::Spaced,
        SandboxAction::Update,
    ];

    for action in internal_labels {
        // No action writes `created`, so any lifecycle write is visible.
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, "created", "space").await?;
        let sandbox = tracked(pool, sandbox_id).await?;

        let error = run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None)
            .await
            .expect_err("internal labels are not lifecycle actions");

        assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{action:?}");
        assert_eq!(error.to_string(), format!("Unknown action: {action}"));
        assert_eq!(
            row(pool, sandbox_id).await?,
            Some(Row::new("created", "unplaced")),
            "{action:?}"
        );
    }

    for action in internal_labels.into_iter().chain([
        SandboxAction::EnableSnapshots,
        SandboxAction::DisableSnapshots,
    ]) {
        let sandbox_id = SandboxId::new();

        let error = run_action(&controller, pool, sandbox_id, None, action, None)
            .await
            .expect_err("untracked sandboxes accept only runtime actions");

        assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{action:?}");
        assert_eq!(error.to_string(), format!("Unknown action: {action}"));
    }
    Ok(())
}

/// Placement-affecting actions and the in-place `pause` / `checkpoint` are
/// refused with a 409 while the resolved node drains, before any substrate
/// call, and leave the row as it was.
#[tokio::test(flavor = "current_thread")]
async fn drain_gated_actions_conflict_and_leave_the_row() -> TestResult {
    let db = pg_test_db_with_max_connections(3).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let node_id = NodeId::new();
    sqlx::query(sql!(
        "INSERT INTO nodes (id, address, role, managed_by, state, drain_phase, drain_mode)
         VALUES ($1, '10.0.0.1'::INET, 'worker', 'space', 'registered', 'draining', 'forced')"
    ))
    .bind(node_id)
    .execute(pool)
    .await?;

    for (action, lifecycle) in [
        (SandboxAction::Start, "stopped"),
        (SandboxAction::Wake, "paused"),
        (
            SandboxAction::ImplicitWake(ExecutionActivityKind::Access),
            "paused",
        ),
        (SandboxAction::Resume, "paused"),
        (SandboxAction::Restore, "suspended"),
        (SandboxAction::Pause, "started"),
        (SandboxAction::Checkpoint, "started"),
    ] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, lifecycle, "space").await?;
        place_on_node(pool, sandbox_id, node_id).await?;
        let sandbox = tracked(pool, sandbox_id).await?;

        let error = run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None)
            .await
            .expect_err("a draining node refuses placement and in-place snapshot work");

        assert_eq!(error.status(), StatusCode::CONFLICT, "{action:?}");
        assert!(
            error.to_string().contains("draining"),
            "{action:?}: {error}"
        );
        assert_eq!(
            row(pool, sandbox_id).await?,
            Some(Row::new(lifecycle, "on_node")),
            "{action:?}"
        );
    }
    Ok(())
}

/// When the substrate cannot reach a worker, every runtime action fails as
/// unavailable and none of them writes the lifecycle it would have reached.
#[tokio::test(flavor = "current_thread")]
async fn unreachable_runtime_fails_actions_without_writing_lifecycle() -> TestResult {
    let db = pg_test_db_with_max_connections(3).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);

    for (action, lifecycle) in [
        (SandboxAction::Start, "stopped"),
        (SandboxAction::Wake, "paused"),
        (SandboxAction::Suspend, "started"),
        (SandboxAction::Stop, "started"),
        (SandboxAction::Kill, "started"),
        (SandboxAction::Pause, "started"),
        (SandboxAction::Resume, "paused"),
        (SandboxAction::Checkpoint, "started"),
    ] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, lifecycle, "space").await?;
        let sandbox = tracked(pool, sandbox_id).await?;

        let error = run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None)
            .await
            .expect_err("no worker is assigned");

        assert_eq!(
            error.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{action:?}: {error}"
        );
        assert_eq!(
            row(pool, sandbox_id).await?,
            Some(Row::new(lifecycle, "unplaced")),
            "{action:?}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `action` completion writes, against a simulated direct worker.
// ---------------------------------------------------------------------------

/// The wake handler maps the completion CAS to its response. The row is
/// moved after the handler read it, which is the race the CAS exists for:
/// a newer suspend claim is a silent success that keeps the claim, and a
/// teardown or terminal lifecycle is a 500 that keeps that lifecycle.
#[tokio::test(flavor = "current_thread")]
async fn wake_completion_outcome_maps_to_the_wake_response() -> TestResult {
    if !crate::test_support::enter_sim_worker_process() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;

    for moved_to in [
        None,
        Some("suspending"),
        Some("stopping"),
        Some("stopped"),
        Some("deleting"),
        Some("quarantined"),
        Some("failed"),
    ] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, "paused", "space").await?;
        place_on_node(pool, sandbox_id, node_id).await?;
        insert_resident(worker.state(), sandbox_id, "paused").await?;
        let sandbox = tracked(pool, sandbox_id).await?;
        if let Some(lifecycle) = moved_to {
            set_lifecycle(pool, sandbox_id, lifecycle).await?;
        }

        let result = run_action(
            &controller,
            pool,
            sandbox_id,
            Some(&sandbox),
            SandboxAction::Wake,
            None,
        )
        .await;

        let ok_body = serde_json::json!({
            "ok": true,
            "action": "wake",
            "sandbox_id": sandbox_id.to_string(),
        });
        match moved_to {
            None => {
                assert_eq!(result?, ok_body);
                assert_eq!(
                    row(pool, sandbox_id).await?,
                    Some(Row::new("started", "on_node"))
                );
            }
            Some("suspending") => {
                assert_eq!(
                    result?, ok_body,
                    "a newer suspend claim is not a wake failure"
                );
                assert_eq!(
                    row(pool, sandbox_id).await?,
                    Some(Row::new("suspending", "on_node"))
                );
            }
            Some(lifecycle) => {
                let error = result.expect_err("a lost completion CAS fails the wake");
                assert_eq!(
                    error.status(),
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "{lifecycle}"
                );
                assert!(
                    error
                        .to_string()
                        .starts_with("sandbox wake lost the lifecycle CAS"),
                    "{lifecycle}: {error}"
                );
                assert_eq!(
                    row(pool, sandbox_id).await?,
                    Some(Row::new(lifecycle, "on_node"))
                );
            }
        }
    }
    Ok(())
}

/// The terminal sandbox events (anything but `*_requested`) emitted for each
/// sandbox while installed as the thread's default subscriber.
#[derive(Clone, Default)]
struct TerminalEventCapture {
    events: std::sync::Arc<std::sync::Mutex<Vec<(String, TerminalEvent)>>>,
}

/// The fields of a terminal sandbox event that describe its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalEvent {
    event_type: String,
    reason: Option<String>,
    lifecycle_state: Option<String>,
    to_state: Option<String>,
    target_state: Option<String>,
}

impl TerminalEventCapture {
    fn take_for(&self, sandbox_id: SandboxId) -> Vec<TerminalEvent> {
        let sandbox_id = sandbox_id.to_string();
        let mut events = self.events.lock().unwrap();
        let (taken, kept): (Vec<_>, Vec<_>) = events
            .drain(..)
            .partition(|(event_sandbox, _)| *event_sandbox == sandbox_id);
        *events = kept;
        taken.into_iter().map(|(_, event)| event).collect()
    }
}

impl<S> tracing_subscriber::Layer<S> for TerminalEventCapture
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        use pplx_space_observability::test_capture::{RecordingVisitor, field_value};
        let mut visitor = RecordingVisitor::default();
        event.record(&mut visitor);
        let fields = visitor.into_fields();
        let (Some("space.sandbox"), Some(event_type), Some(sandbox_id)) = (
            field_value(&fields, "event_stream"),
            field_value(&fields, "event_type"),
            field_value(&fields, "sandbox_id"),
        ) else {
            return;
        };
        if event_type.ends_with("_requested") {
            return;
        }
        let detail = |name| field_value(&fields, name).map(str::to_owned);
        self.events.lock().unwrap().push((
            sandbox_id.to_owned(),
            TerminalEvent {
                event_type: event_type.to_owned(),
                reason: detail("reason"),
                lifecycle_state: detail("lifecycle_state"),
                to_state: detail("to_state"),
                target_state: detail("target_state"),
            },
        ));
    }
}

/// Whether a completed action's post-result write replaces the lifecycle it
/// finds, on the direct substrate, stated independently of the controller.
/// Every outcome reports the action's success; only the row differs.
///
/// - R1: a value only a newer control-plane intent writes, where that intent
///   can still complete on the runtime after this action's result, is kept.
/// - R2: a newer intent this action's result made impossible on the runtime
///   loses to the result.
/// - R3: a value node publication also writes as an observation is replaced;
///   the next publication corrects a wrong replacement, nothing corrects a
///   wrong refusal.
/// - R4: terminal states are kept, as before.
fn post_result_keeps(action: SandboxAction, found: &str) -> bool {
    let starts = matches!(action, SandboxAction::Start | SandboxAction::Resume);
    let pauses = action == SandboxAction::Pause;
    let suspends = action == SandboxAction::Suspend;
    match found
        .trim()
        .to_ascii_lowercase()
        .parse::<SandboxLifecycle>()
    {
        // `deleted` and `cleanup_required` are terminal (R4); any other
        // unknown value is written over.
        Err(_) => matches!(
            found.trim().to_ascii_lowercase().as_str(),
            "deleted" | "cleanup_required"
        ),
        Ok(lifecycle) => match lifecycle {
            SandboxLifecycle::Failed | SandboxLifecycle::Quarantined => true,
            // R1: a committed delete can still complete, and its
            // compensation expects `deleting` (when the delete defers or fails,
            // that compensation restores the lifecycle the delete replaced).
            SandboxLifecycle::Deleting => true,
            // R1 for start, resume and pause: the suspend can still run after
            // them. R2 for stop and kill: a successful stop leaves no suspend
            // able to complete. A suspend completes its own claim.
            SandboxLifecycle::Suspending => starts || pauses,
            // R1: a wake's restore claim that landed after the suspend's
            // runtime call can still restore. A start or stop may have claimed
            // `restoring` itself, and a pause's overwrite is converged to
            // `started` by the wake's own completion.
            SandboxLifecycle::Restoring => suspends,
            // R3: node publication writes both (direct has no Kubernetes
            // suspend fence).
            SandboxLifecycle::Stopped | SandboxLifecycle::Suspended => false,
            // No writer claims `stopping` ahead of a stop; base behavior.
            SandboxLifecycle::Stopping => false,
            SandboxLifecycle::Created
            | SandboxLifecycle::Creating
            | SandboxLifecycle::Starting
            | SandboxLifecycle::Started
            | SandboxLifecycle::Pausing
            | SandboxLifecycle::Paused
            | SandboxLifecycle::Resuming
            | SandboxLifecycle::Checkpointing => false,
        },
    }
}

/// Every runtime action that reports a resulting lifecycle (start, resume,
/// pause, stop, kill, suspend) writes it with a decision taken on the locked
/// row, swept over every stored lifecycle (the variants, the non-variant
/// values, and unnormalized spellings) in two positions:
///
/// - landed while the runtime call was in flight, after the handler read
///   the row in the action's usual starting lifecycle;
/// - already stored when the action was admitted.
///
/// The action succeeds either way, as it always has: a kept lifecycle belongs
/// to a newer intent. A kept lifecycle keeps the placement too; only start
/// writes the placement (`on_node`), so the row is seeded `unplaced` with its
/// active node still assigned.
///
/// Every cell ends the action's sandbox event timeline with its
/// `*_completed` event: a written result carries `to_state`; a kept one
/// carries the kept `lifecycle_state`, the unrecorded `target_state`, and why
/// it was kept (`admitted_from_refused` for a terminal lifecycle the row
/// already held at admission, `post_action_superseded` otherwise).
#[tokio::test(flavor = "current_thread")]
async fn completed_actions_keep_a_conflicting_lifecycle_that_landed_mid_flight() -> TestResult {
    use tracing_subscriber::layer::SubscriberExt as _;

    if !crate::test_support::enter_sim_worker_process_with_owned_token_files() {
        return Ok(());
    }
    let capture = TerminalEventCapture::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(capture.clone()));
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;

    let stored_lifecycles = SandboxLifecycle::all()
        .iter()
        .map(|lifecycle| lifecycle.as_str())
        .chain(NON_VARIANT_LIFECYCLES)
        // Unnormalized spellings, and a value that is no lifecycle at all.
        .chain([" Deleting ", "SUSPENDING", "Stopped ", "hibernated"])
        .collect::<Vec<_>>();
    let mut mismatches = Vec::new();
    for (action, usual, resident, completed) in [
        (SandboxAction::Start, "stopped", "stopped", "started"),
        (SandboxAction::Resume, "paused", "paused", "started"),
        (SandboxAction::Pause, "started", "running", "paused"),
        (SandboxAction::Stop, "started", "running", "stopped"),
        (SandboxAction::Kill, "started", "running", "stopped"),
        (SandboxAction::Suspend, "started", "running", "suspended"),
    ] {
        let completed_placement = if action == SandboxAction::Start {
            "on_node"
        } else {
            "unplaced"
        };
        // (admitted, landed mid-flight). The `None` row is the accepting
        // control: nothing moved, and the result is written.
        let landed = std::iter::once((usual, None))
            .chain(stored_lifecycles.iter().map(|&found| (usual, Some(found))));
        let admitted_from = stored_lifecycles
            .iter()
            .filter(|found| found.trim() == **found && found.to_ascii_lowercase() == **found)
            .map(|&admitted| (admitted, None));
        for (admitted, landed) in landed.chain(admitted_from) {
            let sandbox_id = SandboxId::new();
            insert_sandbox(pool, sandbox_id, admitted, "space").await?;
            place_on_node(pool, sandbox_id, node_id).await?;
            set_placement(pool, sandbox_id, "unplaced").await?;
            insert_resident(worker.state(), sandbox_id, resident).await?;
            let sandbox = tracked(pool, sandbox_id).await?;
            if let Some(lifecycle) = landed {
                set_lifecycle(pool, sandbox_id, lifecycle).await?;
            }
            let found = landed.unwrap_or(admitted);

            let result =
                run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None).await;

            let observed_response = match result {
                Ok(body)
                    if body
                        == serde_json::json!({
                            "ok": true,
                            "action": action.as_str(),
                            "sandbox_id": sandbox_id.to_string(),
                        }) =>
                {
                    "ok".to_owned()
                }
                Ok(body) => format!("unexpected body {body}"),
                Err(error) => format!("{}: {error}", error.status()),
            };
            let observed = (
                observed_response,
                row(pool, sandbox_id).await?,
                capture.take_for(sandbox_id),
            );
            let completed_event = action
                .completed_event()
                .expect("runtime actions have a completed event")
                .to_string();
            let (expected_row, expected_event) = if post_result_keeps(action, found) {
                let normalized = found.trim().to_ascii_lowercase();
                let admitted_terminal = normalized == admitted
                    && matches!(
                        normalized.as_str(),
                        "failed" | "quarantined" | "deleted" | "cleanup_required"
                    );
                (
                    Row::new(found, "unplaced"),
                    TerminalEvent {
                        event_type: completed_event,
                        reason: Some(
                            if admitted_terminal {
                                "admitted_from_refused"
                            } else {
                                "post_action_superseded"
                            }
                            .to_owned(),
                        ),
                        lifecycle_state: Some(found.to_owned()),
                        to_state: None,
                        target_state: Some(completed.to_owned()),
                    },
                )
            } else {
                (
                    Row::new(completed, completed_placement),
                    TerminalEvent {
                        event_type: completed_event,
                        reason: None,
                        lifecycle_state: None,
                        to_state: Some(completed.to_owned()),
                        target_state: None,
                    },
                )
            };
            let expected = ("ok", Some(expected_row), vec![expected_event]);
            if (observed.0.as_str(), &observed.1, &observed.2)
                != (expected.0, &expected.1, &expected.2)
            {
                mismatches.push(format!(
                    "{action:?} admitted from {admitted:?}, found {found:?}: \
                     expected {expected:?}, observed {observed:?}"
                ));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    Ok(())
}

/// A DELETE that persists its intent while a runtime action is in flight
/// wins over that action's result. The action still reports its success,
/// `deleting` and the placement stay as the delete left them, and the
/// delete's own compensation, which expects `deleting`, still restores the
/// lifecycle it replaced.
#[tokio::test(flavor = "current_thread")]
async fn completed_actions_never_clear_a_delete_intent_that_landed_mid_flight() -> TestResult {
    if !crate::test_support::enter_sim_worker_process_with_owned_token_files() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;

    #[derive(Debug, PartialEq, Eq)]
    struct Outcome {
        action: SandboxAction,
        response: Result<(), StatusCode>,
        after_action: Option<Row>,
        compensation: Result<(), StatusCode>,
        after_compensation: Option<Row>,
    }

    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for (action, prior, resident) in [
        (SandboxAction::Start, "stopped", "stopped"),
        (SandboxAction::Resume, "paused", "paused"),
        (SandboxAction::Pause, "started", "running"),
        (SandboxAction::Stop, "started", "running"),
        (SandboxAction::Kill, "started", "running"),
        (SandboxAction::Suspend, "started", "running"),
    ] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, prior, "space").await?;
        place_on_node(pool, sandbox_id, node_id).await?;
        set_placement(pool, sandbox_id, "unplaced").await?;
        insert_resident(worker.state(), sandbox_id, resident).await?;
        let sandbox = tracked(pool, sandbox_id).await?;
        let replaced = begin_delete_against_active_fork(pool, sandbox_id)
            .await?
            .expect("sandbox row exists");

        let response = run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None)
            .await
            .map(|_| ())
            .map_err(|error| error.status());
        let after_action = row(pool, sandbox_id).await?;
        let compensation = restore_lifecycle_after_failed_delete(pool, sandbox_id, replaced)
            .await
            .map_err(|error| error.status());
        observed.push(Outcome {
            action,
            response,
            after_action,
            compensation,
            after_compensation: row(pool, sandbox_id).await?,
        });
        expected.push(Outcome {
            action,
            response: Ok(()),
            after_action: Some(Row::new("deleting", "unplaced")),
            compensation: Ok(()),
            after_compensation: Some(Row::new(prior, "unplaced")),
        });
    }
    assert_eq!(observed, expected);
    Ok(())
}

/// A pause or resume that completes while a node-drain suspend claims the
/// row keeps the claim, and retargets the drain's later restore to the
/// action's result: the runtime ran the action before the suspend
/// (Resuming/Pausing -> Suspending is not a runtime transition), so restoring
/// to the claim's pre-action lifecycle would undo the user's action. An HTTP
/// suspend claim carries no restore target and is left without one. The
/// action was admitted before the drain began, so its node is not draining
/// here; the post-result write does not consult drain state.
#[tokio::test(flavor = "current_thread")]
async fn pause_and_resume_retarget_a_drain_claim_they_keep() -> TestResult {
    if !crate::test_support::enter_sim_worker_process_with_owned_token_files() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;

    for (action, prior, resident, claimed_target, expected_target) in [
        (
            SandboxAction::Pause,
            "started",
            "running",
            Some("started"),
            Some("paused"),
        ),
        (
            SandboxAction::Resume,
            "paused",
            "paused",
            Some("paused"),
            Some("started"),
        ),
        (SandboxAction::Pause, "started", "running", None, None),
        (SandboxAction::Resume, "paused", "paused", None, None),
    ] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, prior, "space").await?;
        place_on_node(pool, sandbox_id, node_id).await?;
        insert_resident(worker.state(), sandbox_id, resident).await?;
        let sandbox = tracked(pool, sandbox_id).await?;
        sqlx::query(sql!(
            "UPDATE sandboxes
                SET lifecycle = 'suspending', evacuation_restore_target = $2
              WHERE id = $1"
        ))
        .bind(sandbox_id)
        .bind(claimed_target)
        .execute(pool)
        .await?;

        let body = run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None).await?;

        assert_eq!(body["ok"], true, "{action:?} over {claimed_target:?}");
        let stored: (String, Option<String>) = sqlx::query_as(sql!(
            "SELECT lifecycle, evacuation_restore_target FROM sandboxes WHERE id = $1"
        ))
        .bind(sandbox_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            stored,
            ("suspending".to_owned(), expected_target.map(str::to_owned)),
            "{action:?} over {claimed_target:?}"
        );
    }
    Ok(())
}

/// A successful stop or kill leaves no suspend able to complete on the
/// runtime, so its `stopped` replaces a suspend claim instead of stranding
/// it. The Kubernetes HTTP suspend never reverts its claim when its node call
/// fails, and node publication does not promote `suspending`, so a refused
/// write would leave the row `suspending` until the transient sweeper fails
/// it. For a node-drain claim, leaving `suspending` also clears the drain's
/// `evacuation_restore_target` marker (and its restore backoff), exactly as
/// the drain's own revert would; the drain's later commit and revert are both
/// guarded on `suspending` and become no-ops.
#[tokio::test(flavor = "current_thread")]
async fn stop_and_kill_replace_a_suspend_claim_they_made_impossible() -> TestResult {
    if !crate::test_support::enter_sim_worker_process_with_owned_token_files() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, healthy_node) = start_sim_worker(pool).await?;
    let (draining_worker, draining_node) = start_sim_worker(pool).await?;
    set_drain_phase(pool, draining_node, "draining").await?;

    for action in [SandboxAction::Stop, SandboxAction::Kill] {
        for (claim, node_id, state, evacuation_target) in [
            ("http suspend", healthy_node, worker.state(), None),
            (
                "node-drain suspend",
                draining_node,
                draining_worker.state(),
                Some("started"),
            ),
        ] {
            let sandbox_id = SandboxId::new();
            insert_sandbox(pool, sandbox_id, "started", "space").await?;
            place_on_node(pool, sandbox_id, node_id).await?;
            insert_resident(state, sandbox_id, "running").await?;
            let sandbox = tracked(pool, sandbox_id).await?;
            sqlx::query(sql!(
                "UPDATE sandboxes
                    SET lifecycle = 'suspending',
                        evacuation_restore_target = $2,
                        restore_attempt_count = CASE WHEN $2::TEXT IS NULL THEN 0 ELSE 2 END
                  WHERE id = $1"
            ))
            .bind(sandbox_id)
            .bind(evacuation_target)
            .execute(pool)
            .await?;

            let body =
                run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None).await?;

            assert_eq!(body["ok"], true, "{action:?} over {claim}");
            let stored: (String, Option<String>, i32) = sqlx::query_as(sql!(
                "SELECT lifecycle, evacuation_restore_target, restore_attempt_count
                   FROM sandboxes WHERE id = $1"
            ))
            .bind(sandbox_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(
                stored,
                ("stopped".to_owned(), None, 0),
                "{action:?} over {claim}"
            );
        }
    }
    Ok(())
}

/// A native start operation whose lifecycle write finds a `stopped` written
/// while its runtime call was in flight overwrites it, and the lifecycle
/// operation executor reports success without asking for a retry, so no
/// second node start is issued. `stopped` is a value node publication also
/// writes, so the start's `started` stands: publication converges a running
/// row the node reports stopped
/// (`node_publication_converges_to_stopped_but_not_back_on_a_started_report`
/// in the internal API), while a plain `started` report never corrects a
/// `stopped` row whose sandbox runs.
///
/// This drives `SandboxLifecycleOperationExecutor::execute` directly; the
/// loop stands in for the dispatcher's re-execution of a retried operation.
/// The `stopped` is written by a trigger on the start's route-identity
/// receipt, which the direct substrate persists after the node start returns
/// and before the lifecycle write; it changes only the database row, fires
/// for this test's sandbox only, and counts the node starts that completed.
#[tokio::test(flavor = "current_thread")]
async fn native_start_overwrites_a_stopped_written_mid_flight_without_a_retry() -> TestResult {
    use crate::public_operations::{ApiProduct, ClaimedPublicOperation};
    use crate::public_sandbox_operations::{
        SandboxLifecycleOperationExecutor, SandboxLifecycleOperationPayload,
    };
    use pplx_space_public_api::{
        OperationId, OperationKind, OperationState, PublicSandboxId, SandboxLifecycleAction,
    };

    if !crate::test_support::enter_sim_worker_process_with_owned_token_files() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let (worker, node_id) = start_sim_worker(pool).await?;
    let sandbox_id = SandboxId::new();
    insert_sandbox(pool, sandbox_id, "created", "space").await?;
    place_on_node(pool, sandbox_id, node_id).await?;
    insert_resident(worker.state(), sandbox_id, "creating").await?;
    sqlx::query(sql!(
        "CREATE TABLE test_completed_node_starts (sandbox_id UUID PRIMARY KEY, starts INT NOT NULL)"
    ))
    .execute(pool)
    .await?;
    sqlx::query(sql!(
        "INSERT INTO test_completed_node_starts (sandbox_id, starts) VALUES ($1, 0)"
    ))
    .bind(sandbox_id)
    .execute(pool)
    .await?;
    sqlx::query(sql!(
        "CREATE FUNCTION test_stop_after_first_node_start() RETURNS trigger
         LANGUAGE plpgsql AS $$
         DECLARE completed INT;
         BEGIN
             UPDATE test_completed_node_starts
                SET starts = starts + 1
              WHERE sandbox_id = NEW.sandbox_id
             RETURNING starts INTO completed;
             IF completed = 1 THEN
                 UPDATE sandboxes SET lifecycle = 'stopped' WHERE id = NEW.sandbox_id;
             END IF;
             RETURN NEW;
         END $$"
    ))
    .execute(pool)
    .await?;
    sqlx::query(sql!(
        "CREATE TRIGGER test_stop_after_first_node_start
         AFTER INSERT OR UPDATE ON sandbox_direct_route_identities
         FOR EACH ROW EXECUTE FUNCTION test_stop_after_first_node_start()"
    ))
    .execute(pool)
    .await?;

    let operation = ClaimedPublicOperation {
        id: OperationId::new(),
        org_id: "00000000-0000-0000-0000-000000000001".parse()?,
        actor_user_id: Some("00000000-0000-0000-0000-000000000010".parse()?),
        api_product: ApiProduct::Native,
        kind: OperationKind::StartSandbox,
        target_type: None,
        target_id: Some(PublicSandboxId::from(sandbox_id).to_string()),
        payload: SandboxLifecycleOperationPayload {
            sandbox_id: PublicSandboxId::from(sandbox_id),
            action: SandboxLifecycleAction::Start,
            rollback: None,
        }
        .into_json(),
        attempts: 1,
        claim_generation: 1,
    };
    let executor = SandboxLifecycleOperationExecutor::new(pool.clone(), direct_controller(pool));

    // Re-run the operation while it asks for a retry, as the dispatcher would;
    // before a retry the simulated resident is stopped too.
    let mut result = executor.execute(&operation).await;
    let mut executions = 1;
    while result.retry_after.is_some() && executions < 3 {
        worker
            .state()
            .set_status(&sandbox_id.to_string(), "stopped")
            .await?;
        result = executor.execute(&operation).await;
        executions += 1;
    }

    assert_eq!(executions, 1, "the operation was retried: {result:?}");
    assert_eq!(result.state, OperationState::Succeeded, "{result:?}");
    let completed_node_starts: i32 = sqlx::query_scalar(sql!(
        "SELECT starts FROM test_completed_node_starts WHERE sandbox_id = $1"
    ))
    .bind(sandbox_id)
    .fetch_one(pool)
    .await?;
    assert_eq!(completed_node_starts, 1);
    assert_eq!(
        row(pool, sandbox_id).await?,
        Some(Row::new("started", "on_node"))
    );
    Ok(())
}

/// Stop and kill are not drain-gated: on a draining node they still reach
/// the worker and mark the sandbox stopped.
#[tokio::test(flavor = "current_thread")]
async fn stop_and_kill_on_a_draining_node_still_reach_the_worker() -> TestResult {
    if !crate::test_support::enter_sim_worker_process() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;
    set_drain_phase(pool, node_id, "draining").await?;

    for action in [SandboxAction::Stop, SandboxAction::Kill] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, "started", "space").await?;
        place_on_node(pool, sandbox_id, node_id).await?;
        insert_resident(worker.state(), sandbox_id, "running").await?;
        let sandbox = tracked(pool, sandbox_id).await?;

        run_action(&controller, pool, sandbox_id, Some(&sandbox), action, None).await?;

        assert_eq!(
            worker.state().get(&sandbox_id.to_string()).await?.status,
            "stopped",
            "{action:?}"
        );
        assert_eq!(
            row(pool, sandbox_id).await?,
            Some(Row::new("stopped", "on_node")),
            "{action:?}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `untracked_action`: no Postgres row was supplied by the caller.
// ---------------------------------------------------------------------------

/// Without lifecycle context, untracked start and restore validate their
/// activation options first and then charge cluster admission
/// conservatively (cold / restore).
#[tokio::test(flavor = "current_thread")]
async fn untracked_start_and_restore_validate_then_charge_admission() -> TestResult {
    let db = pg_test_db_with_max_connections(3).await?;
    let pool = &db.pool;
    let controller = controller_with_empty_cluster_admission(pool);

    for action in [SandboxAction::Start, SandboxAction::Restore] {
        let invalid = SandboxActivationRequest {
            blocking: None,
            ready_timeout_secs: Some(0),
        };
        let error = run_action(
            &controller,
            pool,
            SandboxId::new(),
            None,
            action,
            Some(invalid),
        )
        .await
        .expect_err("invalid activation options are rejected");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST, "{action:?}");

        let error = run_action(&controller, pool, SandboxId::new(), None, action, None)
            .await
            .expect_err("an empty cluster refuses the activation");
        assert_eq!(error.status(), StatusCode::TOO_MANY_REQUESTS, "{action:?}");
    }
    Ok(())
}

/// The direct substrate resolves a worker only through a placed `sandboxes`
/// row, so an untracked runtime action for an unknown id is unavailable.
#[tokio::test(flavor = "current_thread")]
async fn untracked_runtime_actions_without_placement_are_unavailable() -> TestResult {
    let db = pg_test_db_with_max_connections(3).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);

    for action in [
        SandboxAction::Start,
        SandboxAction::Wake,
        SandboxAction::ImplicitWake(ExecutionActivityKind::Access),
        SandboxAction::Suspend,
        SandboxAction::Stop,
        SandboxAction::Pause,
        SandboxAction::Resume,
        SandboxAction::Kill,
        SandboxAction::Checkpoint,
    ] {
        let sandbox_id = SandboxId::new();
        let error = run_action(&controller, pool, sandbox_id, None, action, None)
            .await
            .expect_err("no worker can be resolved");
        assert_eq!(
            error.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{action:?}: {error}"
        );
    }
    Ok(())
}

/// Untracked actions report the status they reached in the body and never
/// write `sandboxes.lifecycle`, even when a row for the id exists (the
/// caller passed no tracked sandbox, so the controller does not own it).
#[tokio::test(flavor = "current_thread")]
async fn untracked_actions_report_status_and_leave_the_row() -> TestResult {
    if !crate::test_support::enter_sim_worker_process() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(4).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;

    for (action, resident, status, resident_after) in [
        (SandboxAction::Start, "stopped", "started", "running"),
        (SandboxAction::Wake, "paused", "started", "running"),
        (SandboxAction::Stop, "running", "stopped", "stopped"),
        (SandboxAction::Kill, "running", "stopped", "stopped"),
        (SandboxAction::Pause, "running", "paused", "paused"),
        (SandboxAction::Resume, "paused", "started", "running"),
        (SandboxAction::Suspend, "running", "suspended", "suspended"),
    ] {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, "created", "space").await?;
        place_on_node(pool, sandbox_id, node_id).await?;
        insert_resident(worker.state(), sandbox_id, resident).await?;

        let body = run_action(&controller, pool, sandbox_id, None, action, None).await?;

        assert_eq!(
            body,
            serde_json::json!({
                "ok": true,
                "action": action.as_str(),
                "sandbox_id": sandbox_id.to_string(),
                "status": status,
            }),
        );
        assert_eq!(
            worker.state().get(&sandbox_id.to_string()).await?.status,
            resident_after,
            "{action:?}"
        );
        assert_eq!(
            row(pool, sandbox_id).await?,
            Some(Row::new("created", "on_node")),
            "{action:?}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `delete` of an observed-only sandbox (`delete_observed_sandbox`).
// ---------------------------------------------------------------------------

async fn insert_observed_state(
    pool: &PgPool,
    sandbox_id: SandboxId,
    node_id: NodeId,
) -> TestResult {
    sqlx::query(sql!(
        "INSERT INTO sandbox_observed_states (sandbox_id, node_id, status)
         VALUES ($1, $2, 'running')"
    ))
    .bind(sandbox_id)
    .bind(node_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// With no tracked row, `delete` targets the node that last observed the
/// sandbox. An id no node observed is a 404.
#[tokio::test(flavor = "current_thread")]
async fn observed_only_delete_without_an_observation_is_not_found() -> TestResult {
    let db = pg_test_db_with_max_connections(2).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);

    let error = controller
        .delete(pool, SandboxId::new(), None)
        .await
        .expect_err("nothing observed the sandbox");

    assert_eq!(error.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// The worker on the node that observed the sandbox deletes it, and only it.
#[tokio::test(flavor = "current_thread")]
async fn observed_only_delete_removes_the_resident_on_the_observing_node() -> TestResult {
    if !crate::test_support::enter_sim_worker_process() {
        return Ok(());
    }
    let db = pg_test_db_with_max_connections(3).await?;
    let pool = &db.pool;
    let controller = direct_controller(pool);
    let (worker, node_id) = start_sim_worker(pool).await?;
    let sandbox_id = SandboxId::new();
    let bystander = SandboxId::new();
    insert_resident(worker.state(), sandbox_id, "running").await?;
    insert_resident(worker.state(), bystander, "running").await?;
    insert_observed_state(pool, sandbox_id, node_id).await?;

    controller.delete(pool, sandbox_id, None).await?;

    assert!(
        worker.state().get(&sandbox_id.to_string()).await.is_err(),
        "the observed resident is deleted on its node"
    );
    assert_eq!(
        worker.state().get(&bystander.to_string()).await?.status,
        "running"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// `wake_completion_redrive`: the cancelled-wake completion on the direct
// substrate, whose hydration probe always answers routable.
// ---------------------------------------------------------------------------

async fn redrive(pool: &PgPool, sandbox_id: SandboxId) {
    // A zero budget makes exactly one attempt: read, probe, fenced CAS.
    wake_completion_redrive(
        pool.clone(),
        direct_substrate(pool),
        sandbox_id,
        WakeCompletionAction::Wake,
        std::time::Duration::ZERO,
    )
    .await;
}

/// The redrive claims only a `restoring` row; any other prior lifecycle
/// (including the non-variant values) is left exactly as found.
#[tokio::test(flavor = "current_thread")]
async fn redrive_completes_only_a_restoring_row() -> TestResult {
    let db = pg_test_db_with_max_connections(2).await?;
    let pool = &db.pool;

    let lifecycles = SandboxLifecycle::all()
        .iter()
        .map(|lifecycle| lifecycle.as_str())
        .chain(NON_VARIANT_LIFECYCLES);
    for lifecycle in lifecycles {
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, lifecycle, "space").await?;

        redrive(pool, sandbox_id).await;

        let expected = if lifecycle == SandboxLifecycle::Restoring.as_str() {
            Row::new("started", "on_node")
        } else {
            Row::new(lifecycle, "unplaced")
        };
        assert_eq!(row(pool, sandbox_id).await?, Some(expected), "{lifecycle}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Delete-intent compensation.
// ---------------------------------------------------------------------------

/// Restoring the pre-delete lifecycle is a CAS on `deleting`: if anything
/// moved the row meanwhile it refuses with a 500 and leaves the row alone.
/// The success arm is pinned by `deferred_delete_restores_the_previous_lifecycle`.
#[tokio::test(flavor = "current_thread")]
async fn failed_delete_compensation_refuses_a_row_that_left_deleting() -> TestResult {
    let db = pg_test_db_with_max_connections(2).await?;
    let pool = &db.pool;

    let moved_to = SandboxLifecycle::all()
        .iter()
        .filter(|lifecycle| **lifecycle != SandboxLifecycle::Deleting)
        .map(|lifecycle| lifecycle.as_str())
        .chain(NON_VARIANT_LIFECYCLES);
    for current in moved_to {
        // A restoration target that differs from the current lifecycle, so a
        // write that slipped past the CAS would be visible.
        let target = if current == SandboxLifecycle::Paused.as_str() {
            SandboxLifecycle::Started
        } else {
            SandboxLifecycle::Paused
        };
        let sandbox_id = SandboxId::new();
        insert_sandbox(pool, sandbox_id, current, "space").await?;

        let error = restore_lifecycle_after_failed_delete(pool, sandbox_id, target)
            .await
            .expect_err("the row left `deleting`");

        assert_eq!(
            error.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{current}"
        );
        assert_eq!(
            row(pool, sandbox_id).await?,
            Some(Row::new(current, "unplaced")),
            "{current}"
        );
    }
    Ok(())
}
