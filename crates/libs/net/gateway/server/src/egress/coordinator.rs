use pplx_space_net_common::{
    EffectiveNetworkPolicyRef, NetworkPolicyV1, TlsInterception,
    effective_network_policy_ref_authority_label,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, watch};
// Use the same clock as the pending-hold timers, including paused-time tests.
use super::attribution::DnsAllowCache;
use super::policy::*;
use super::startup_timing::StartupTiming;
use super::tracker::DnsAttributedTcpTracker;
use crate::proto::NetworkPolicyErrorKind;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

pub(super) struct Coordinator {
    state_tx: watch::Sender<Arc<NetworkStateInner>>,
    state_rx: watch::Receiver<Arc<NetworkStateInner>>,
    writer_lock: Mutex<()>,
    refresh_lock: Mutex<()>,
    pending_tcp_connections: Arc<AtomicU64>,
    pub(super) dns_allow_cache: DnsAllowCache,
    pub(super) tcp_connections: DnsAttributedTcpTracker,
    pub(super) reserved_dns_cancellation: ReservedDnsCancellation,
    traffic_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    readiness_observer: Arc<StartupTiming>,
}

struct TcpPendingWaiterGuard {
    pending_tcp_connections: Arc<AtomicU64>,
    active: bool,
}

impl TcpPendingWaiterGuard {
    fn finish(&mut self) {
        if self.active {
            let update_result = self.pending_tcp_connections.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |current| Some(current.saturating_sub(1)),
            );
            debug_assert!(update_result.is_ok());
            self.active = false;
        }
    }
}

impl Drop for TcpPendingWaiterGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Coordinator {
    fn new(initial: InitialPolicyState, traffic_gate: Option<TrafficGate>) -> Self {
        let (lifecycle, installed_policy_ref) = match initial {
            InitialPolicyState::NoIdentity => (
                PolicyLifecycle::NoIdentity,
                Some(EffectiveNetworkPolicyRef::no_identity()),
            ),
            InitialPolicyState::PendingPolicy => {
                (PolicyLifecycle::Pending { previous: None }, None)
            }
        };
        let inner = Arc::new(NetworkStateInner {
            lifecycle,
            diagnostics: HydrationDiagnostics {
                installed_policy_ref,
                last_transition_unix_ms: unix_time_ms(),
                ..Default::default()
            },
        });
        let readiness_observer = Arc::new(StartupTiming::new(inner.clone()));
        let (state_tx, state_rx) = watch::channel(inner);
        Self {
            traffic_gate,
            readiness_observer,
            state_tx,
            state_rx,
            writer_lock: Mutex::new(()),
            refresh_lock: Mutex::new(()),
            pending_tcp_connections: Arc::new(AtomicU64::new(0)),
            dns_allow_cache: DnsAllowCache::default(),
            tcp_connections: DnsAttributedTcpTracker::default(),
            reserved_dns_cancellation: ReservedDnsCancellation::default(),
        }
    }

    pub async fn snapshot(&self) -> NetworkStateSnapshot {
        let inner = self.state_rx.borrow().clone();
        self.snapshot_from_inner(inner.as_ref())
    }

    /// Observer for the interception dependency (gateway CA).
    /// Feeds startup timing independently of policy readiness.
    pub fn interception_readiness_observer(&self) -> ReadinessObserver {
        // Capture only the policy receiver, not the gateway/authority graph.
        // Capturing self would create a cycle through traffic_gate.
        let state = self.state_rx.clone();
        let observer = self.readiness_observer.clone();
        Arc::new(move |ready| {
            observer.observe(|| (state.borrow().clone(), ready));
        })
    }

    /// Live interception readiness for `policy`, evaluated against the
    /// traffic gate at call time.
    fn interception_readiness_for(
        &self,
        policy: Option<&NetworkPolicyV1>,
    ) -> InterceptionReadiness {
        match policy {
            Some(policy) if policy.tls_interception == TlsInterception::On => {
                if self.traffic_gate_open() {
                    InterceptionReadiness::Ready
                } else {
                    InterceptionReadiness::DependenciesNotReady
                }
            }
            _ => InterceptionReadiness::NotRequired,
        }
    }

    fn traffic_gate_open(&self) -> bool {
        self.traffic_gate.as_ref().is_none_or(|gate| gate())
    }

    fn policy_traffic_ready(&self, policy: &NetworkPolicyV1) -> bool {
        policy.tls_interception == TlsInterception::Off || self.traffic_gate_open()
    }

    /// Whether `expected` is the exact policy generation currently ready for
    /// new TCP admission. Each successful install allocates a new policy, so
    /// pointer identity is the generation token without duplicating state.
    pub(crate) fn is_current_tcp_policy(&self, expected: &Arc<NetworkPolicyV1>) -> bool {
        let inner = self.state_rx.borrow();
        inner.lifecycle.ready().is_some_and(|installed| {
            Arc::ptr_eq(&installed.policy, expected) && self.policy_traffic_ready(&installed.policy)
        })
    }

    /// Keep raw values inside the checked snapshot borrow; callers may only
    /// return an owned representation prepared synchronously by this closure.
    pub(crate) fn with_network_credentials<T>(
        &self,
        expected_policy: &Arc<NetworkPolicyV1>,
        use_credentials: impl FnOnce(&NetworkCredentialSnapshot) -> T,
    ) -> Result<T, NetworkCredentialSnapshotError> {
        let inner = self.state_rx.borrow();
        match &inner.lifecycle {
            PolicyLifecycle::Pending {
                previous: Some(installed),
            } if Arc::ptr_eq(&installed.policy, expected_policy) => {
                Err(NetworkCredentialSnapshotError::TemporarilyUnavailable)
            }
            PolicyLifecycle::Ready(installed)
                if Arc::ptr_eq(&installed.policy, expected_policy)
                    && self.policy_traffic_ready(expected_policy) =>
            {
                Ok(use_credentials(&installed.credentials))
            }
            _ => Err(NetworkCredentialSnapshotError::Stale),
        }
    }

    fn spawn_hydration(
        self: &Arc<Self>,
        loader: PolicyLoader,
        shutdown_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let state = self.clone();
        tokio::spawn(async move {
            state.run_hydration_loop(loader, shutdown_token).await;
        })
    }

    async fn run_hydration_loop(
        self: Arc<Self>,
        loader: PolicyLoader,
        shutdown_token: CancellationToken,
    ) {
        let mut retry_delay = INITIAL_HYDRATION_RETRY_INTERVAL;
        let mut attempts = 0_u64;

        loop {
            if shutdown_token.is_cancelled() {
                break;
            }

            let recheck = {
                let current = self.state_rx.borrow();
                match &current.lifecycle {
                    PolicyLifecycle::Ready(_) => {
                        Some((Duration::ZERO, READY_HYDRATION_RECHECK_INTERVAL))
                    }
                    PolicyLifecycle::PolicyError(failure)
                        if matches!(
                            failure.as_ref(),
                            HydrationFailure::ReferenceMismatch { .. }
                        ) =>
                    {
                        Some((
                            POLICY_REF_MISMATCH_RECHECK_INTERVAL,
                            POLICY_REF_MISMATCH_RECHECK_INTERVAL,
                        ))
                    }
                    PolicyLifecycle::NoIdentity
                    | PolicyLifecycle::Draining { .. }
                    | PolicyLifecycle::Stopped => break,
                    PolicyLifecycle::Pending { .. } | PolicyLifecycle::PolicyError(_) => None,
                }
            };
            if let Some((retry_backoff, interval)) = recheck {
                attempts = 0;
                retry_delay = INITIAL_HYDRATION_RETRY_INTERVAL;
                self.set_retry_backoff(retry_backoff).await;
                if self
                    .wait_for_state_change_or_shutdown(interval, &shutdown_token)
                    .await
                {
                    break;
                }
                continue;
            }

            if attempts >= MAX_SELF_HYDRATION_RETRIES {
                self.mark_permanent_policy_error(attempts).await;
                break;
            }

            attempts = attempts.saturating_add(1);
            self.set_retry_backoff(retry_delay).await;
            pplx_space_metrics::gateway::policy_hydration_retry(
                NetworkStateReason::SelfRetry.as_str(),
            );
            let snapshot = self
                .refresh(loader.clone(), None, NetworkStateReason::SelfRetry)
                .await;
            if self.state_rx.borrow().lifecycle.ready().is_some() {
                attempts = 0;
                retry_delay = INITIAL_HYDRATION_RETRY_INTERVAL;
                continue;
            }

            warn!(target: "pplx_space_net_gateway::state",
                error_kind = snapshot.last_error_kind.as_str(),
                consecutive_hydration_failures = snapshot.consecutive_hydration_failures,
                current_retry_backoff_ms =
                    u64::try_from(retry_delay.as_millis()).unwrap_or(u64::MAX),
                "net-gateway policy hydration retry did not reach ready"
            );

            if wait_for_shutdown(retry_delay, &shutdown_token).await {
                break;
            }
            retry_delay = retry_delay
                .saturating_mul(2)
                .min(MAX_HYDRATION_RETRY_BACKOFF);
        }
    }

    async fn wait_for_state_change_or_shutdown(
        &self,
        duration: Duration,
        shutdown_token: &CancellationToken,
    ) -> bool {
        let mut state_rx = self.state_tx.subscribe();
        tokio::select! {
            _ = shutdown_token.cancelled() => true,
            _ = tokio::time::sleep(duration) => false,
            changed = state_rx.changed() => changed.is_err(),
        }
    }

    async fn update_inner(
        &self,
        update: impl FnOnce(&mut NetworkStateInner),
    ) -> Arc<NetworkStateInner> {
        let _guard = self.writer_lock.lock().await;
        let current = self.state_rx.borrow().clone();
        let mut inner = current.as_ref().clone();
        update(&mut inner);
        if current.lifecycle.ready().is_some() && inner.lifecycle.ready().is_none() {
            self.reserved_dns_cancellation.cancel_in_flight();
        }
        let policy_removed =
            current.lifecycle.installed().is_some() && inner.lifecycle.installed().is_none();
        let inner = Arc::new(inner);
        self.state_tx.send_replace(inner.clone());
        self.readiness_observer
            .observe(|| (self.state_rx.borrow().clone(), self.traffic_gate_open()));
        // Publish unavailability before sweeping. An old-policy admission racing
        // the transition then either fails its final generation check or was
        // already registered and is cancelled below.
        if policy_removed {
            let cancelled = self.tcp_connections.cancel_all();
            if cancelled > 0 {
                info!(target: "pplx_space_net_gateway::state",
                    dns_attributed_cancelled_tcp_connections = cancelled,
                    "cancelled TCP connections after network policy invalidation"
                );
            }
        }
        inner
    }

    pub async fn set_retry_backoff(&self, retry_backoff: Duration) {
        self.update_inner(|inner| {
            inner.diagnostics.current_retry_backoff_ms =
                u64::try_from(retry_backoff.as_millis()).unwrap_or(u64::MAX);
        })
        .await;
    }

    async fn mark_permanent_policy_error(&self, attempts: u64) -> NetworkStateSnapshot {
        let inner = self
            .update_inner(|inner| {
                set_lifecycle(
                    inner,
                    PolicyLifecycle::PolicyError(Arc::new(HydrationFailure::RetryExhausted {
                        attempts,
                    })),
                    NetworkStateReason::SelfRetry,
                );
                inner.diagnostics.current_retry_backoff_ms = 0;
                inner.diagnostics.consecutive_hydration_failures = inner
                    .diagnostics
                    .consecutive_hydration_failures
                    .saturating_add(1);
                pplx_space_metrics::gateway::policy_hydration_error("permanent");
                record_hydration_failures(
                    inner.lifecycle.error_kind(),
                    inner.diagnostics.consecutive_hydration_failures,
                );
            })
            .await;
        self.snapshot_from_inner(inner.as_ref())
    }

    pub async fn set_no_identity(&self, reason: NetworkStateReason) -> NetworkStateSnapshot {
        let inner = self
            .update_inner(|inner| {
                if let Some(installed) = inner.lifecycle.installed() {
                    info!(target: "pplx_space_net_gateway::state",
                        cleared_policy_ref = %installed.reference,
                        reason = reason.as_str(),
                        "net-gateway policy cleared"
                    );
                }
                set_lifecycle(inner, PolicyLifecycle::NoIdentity, reason);
                inner.diagnostics.installed_policy_ref =
                    Some(EffectiveNetworkPolicyRef::no_identity());
                inner.diagnostics.current_retry_backoff_ms = 0;
                inner.diagnostics.consecutive_hydration_failures = 0;
                pplx_space_metrics::gateway::policy_ref(
                    "no_identity",
                    inner.lifecycle.mode().as_str(),
                );
                record_hydration_failures(
                    inner.lifecycle.error_kind(),
                    inner.diagnostics.consecutive_hydration_failures,
                );
            })
            .await;
        self.snapshot_from_inner(inner.as_ref())
    }

    /// Re-pull and reconcile the network policy. Skips the pull when the gateway
    /// is already `Ready`, its reference satisfies the expectation, and no
    /// network credentials need to be refreshed.
    pub async fn refresh(
        &self,
        loader: PolicyLoader,
        expected_policy_ref: Option<EffectiveNetworkPolicyRef>,
        reason: NetworkStateReason,
    ) -> NetworkStateSnapshot {
        let _guard = self.refresh_lock.lock().await;

        let current = self.state_rx.borrow().clone();
        if current.lifecycle.ready().is_some_and(|installed| {
            expected_policy_ref
                .as_ref()
                .is_none_or(|expected| &installed.reference == expected)
                && installed.policy.network_credential_rules.is_empty()
        }) {
            return self.snapshot_from_inner(current.as_ref());
        }

        let current = self
            .update_inner(|inner| {
                if matches!(
                    inner.lifecycle,
                    PolicyLifecycle::Draining { .. } | PolicyLifecycle::Stopped
                ) {
                    return;
                }
                let previous = inner.lifecycle.installed().cloned();
                set_lifecycle(inner, PolicyLifecycle::Pending { previous }, reason);
            })
            .await;
        if matches!(
            current.lifecycle,
            PolicyLifecycle::Draining { .. } | PolicyLifecycle::Stopped
        ) {
            return self.snapshot_from_inner(current.as_ref());
        }

        let started_at = Instant::now();
        let result = tokio::time::timeout(HYDRATION_DEADLINE, (loader)()).await;
        let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = match result {
            Ok(Ok(pulled)) => match expected_policy_ref {
                Some(expected) if expected != pulled.policy_ref => {
                    Err(HydrationFailure::ReferenceMismatch {
                        expected,
                        actual: pulled.policy_ref,
                    })
                }
                _ => Ok(pulled),
            },
            Ok(Err(error)) => Err(HydrationFailure::Load(error)),
            Err(_) => Err(HydrationFailure::Timeout {
                deadline: HYDRATION_DEADLINE,
            }),
        };

        let inner = self
            .update_inner(move |inner| {
                if matches!(
                    inner.lifecycle,
                    PolicyLifecycle::NoIdentity
                        | PolicyLifecycle::Draining { .. }
                        | PolicyLifecycle::Stopped
                ) {
                    return;
                }
                inner.diagnostics.last_hydration_latency_ms = latency_ms;
                match result {
                    Ok(pulled) => {
                        let installed = Arc::new(InstalledPolicy::from(pulled));
                        let policy = &installed.policy;
                        let (action, allow_counts, deny_counts) = policy_log_summary(policy);
                        self.reconcile_policy(policy);
                        set_lifecycle(inner, PolicyLifecycle::Ready(installed.clone()), reason);
                        inner.diagnostics.installed_policy_ref = Some(installed.reference.clone());
                        inner.diagnostics.current_retry_backoff_ms = 0;
                        inner.diagnostics.consecutive_hydration_failures = 0;
                        pplx_space_metrics::gateway::policy_hydration_latency(
                            "success",
                            latency_ms as f64,
                        );
                        pplx_space_metrics::gateway::policy_ref(
                            installed.reference.authority_label(),
                            inner.lifecycle.mode().as_str(),
                        );
                        info!(target: "pplx_space_net_gateway::state",
                            installed_policy_ref = %installed.reference,
                            latency_ms,
                            policy_version = "v1",
                            action,
                            allow_rules = allow_counts.total,
                            allow_domains = allow_counts.domains,
                            allow_wildcard_domains = allow_counts.wildcard_domains,
                            allow_ips = allow_counts.ips,
                            allow_cidrs = allow_counts.cidrs,
                            deny_rules = deny_counts.total,
                            deny_domains = deny_counts.domains,
                            deny_wildcard_domains = deny_counts.wildcard_domains,
                            deny_ips = deny_counts.ips,
                            deny_cidrs = deny_counts.cidrs,
                            reason = reason.as_str(),
                            // Effective mode for this install. Every
                            // install (refresh, self-hydration, restore)
                            // logs this line, so it tracks mode changes.
                            tls_interception_mode = policy.tls_interception.as_str(),
                            // The rendered destination strings are NOT
                            // logged: policy destinations are tenant-
                            // authored and a hostname can carry arbitrary
                            // encoded data, so emitting them on every
                            // hydration would be an ungated log leak. The
                            // per-class counts above are the bounded,
                            // non-sensitive shape signal.
                            "net-gateway policy hydration ready"
                        );
                    }
                    Err(failure) => {
                        let failure = Arc::new(failure);
                        set_lifecycle(inner, PolicyLifecycle::PolicyError(failure.clone()), reason);
                        inner.diagnostics.consecutive_hydration_failures = inner
                            .diagnostics
                            .consecutive_hydration_failures
                            .saturating_add(1);
                        match failure.as_ref() {
                            HydrationFailure::ReferenceMismatch { expected, actual } => {
                                pplx_space_metrics::gateway::policy_hydration_error(
                                    "policy_ref_mismatch",
                                );
                                pplx_space_metrics::gateway::policy_ref(
                                    actual.authority_label(),
                                    inner.lifecycle.mode().as_str(),
                                );
                                warn!(target: "pplx_space_net_gateway::state",
                                    expected_policy_ref = %expected,
                                    installed_policy_ref = %actual,
                                    reason = reason.as_str(),
                                    "net-gateway policy hydration returned mismatched policy"
                                );
                            }
                            HydrationFailure::Load(
                                HydrateNetworkStateError::InvalidReference { .. },
                            ) => {
                                pplx_space_metrics::gateway::policy_hydration_error(
                                    "invalid_policy_ref",
                                );
                                pplx_space_metrics::gateway::policy_hydration_latency(
                                    "error",
                                    latency_ms as f64,
                                );
                                warn!(target: "pplx_space_net_gateway::state",
                                    last_error = %failure,
                                    latency_ms,
                                    reason = reason.as_str(),
                                    "net-gateway policy hydration returned invalid policy ref"
                                );
                            }
                            HydrationFailure::Load(_) | HydrationFailure::RetryExhausted { .. } => {
                                pplx_space_metrics::gateway::policy_hydration_error("loader_error");
                                pplx_space_metrics::gateway::policy_hydration_latency(
                                    "error",
                                    latency_ms as f64,
                                );
                                warn!(target: "pplx_space_net_gateway::state",
                                    last_error = %failure,
                                    latency_ms,
                                    reason = reason.as_str(),
                                    "net-gateway policy hydration failed"
                                );
                            }
                            HydrationFailure::Timeout { .. } => {
                                pplx_space_metrics::gateway::policy_hydration_error("timeout");
                                pplx_space_metrics::gateway::policy_hydration_latency(
                                    "timeout",
                                    latency_ms as f64,
                                );
                                warn!(target: "pplx_space_net_gateway::state",
                                    latency_ms,
                                    reason = reason.as_str(),
                                    "net-gateway policy hydration timed out"
                                );
                            }
                        }
                    }
                }
                record_hydration_failures(
                    inner.lifecycle.error_kind(),
                    inner.diagnostics.consecutive_hydration_failures,
                );
            })
            .await;
        self.snapshot_from_inner(inner.as_ref())
    }

    pub async fn wait_for_tcp_policy(&self) -> TrafficPolicy {
        let started_at = Instant::now();
        let mut warned = false;
        let mut pending_waiter = None;
        let deadline = Instant::now() + TCP_PENDING_TIMEOUT;
        let mut state_rx = self.state_rx.clone();

        loop {
            let inner = state_rx.borrow().clone();
            let snapshot = self.snapshot_from_inner(&inner);
            match &inner.lifecycle {
                PolicyLifecycle::Ready(installed) => {
                    if pending_waiter.take().is_some() {
                        record_tcp_pending_wait("ready", started_at);
                    }
                    if snapshot.interception_readiness.blocks_admission() {
                        record_not_ready_denial("tcp", PolicyNotReady::InterceptionNotReady);
                        return TrafficPolicy::NotReady(PolicyNotReady::InterceptionNotReady);
                    }
                    return TrafficPolicy::Ready(installed.policy.clone());
                }
                PolicyLifecycle::Pending { .. } => {
                    if pending_waiter.is_none() {
                        pending_waiter = Some(self.start_tcp_pending_waiter());
                        pplx_space_metrics::gateway::pending_tcp_wait("started");
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        pending_waiter.take();
                        pplx_space_metrics::gateway::pending_timeout("tcp");
                        record_tcp_pending_wait("timeout", started_at);
                        return TrafficPolicy::NotReady(PolicyNotReady::PendingPolicyTimeout);
                    }
                    if !warned && started_at.elapsed() >= TCP_PENDING_WARN_AFTER {
                        warned = true;
                        record_pending_tcp_slow_wait(&snapshot, started_at);
                    }
                    let remaining = deadline.saturating_duration_since(now);
                    let wait_for = if warned {
                        remaining
                    } else {
                        remaining.min(TCP_PENDING_WARN_AFTER.saturating_sub(started_at.elapsed()))
                    };
                    if tokio::time::timeout(wait_for, state_rx.changed())
                        .await
                        .is_err()
                    {
                        if !warned && started_at.elapsed() >= TCP_PENDING_WARN_AFTER {
                            warned = true;
                            record_pending_tcp_slow_wait(&snapshot, started_at);
                            continue;
                        }
                        pending_waiter.take();
                        pplx_space_metrics::gateway::pending_timeout("tcp");
                        record_tcp_pending_wait("timeout", started_at);
                        return TrafficPolicy::NotReady(PolicyNotReady::PendingPolicyTimeout);
                    }
                }
                PolicyLifecycle::NoIdentity => {
                    if pending_waiter.take().is_some() {
                        record_tcp_pending_wait("no_identity", started_at);
                    }
                    record_not_ready_denial("tcp", PolicyNotReady::NoIdentity);
                    return TrafficPolicy::NotReady(PolicyNotReady::NoIdentity);
                }
                PolicyLifecycle::PolicyError(_) => {
                    if pending_waiter.take().is_some() {
                        record_tcp_pending_wait("policy_error", started_at);
                    }
                    record_not_ready_denial("tcp", PolicyNotReady::PolicyError);
                    return TrafficPolicy::NotReady(PolicyNotReady::PolicyError);
                }
                PolicyLifecycle::Draining { .. } => {
                    if pending_waiter.take().is_some() {
                        record_tcp_pending_wait("draining", started_at);
                    }
                    record_not_ready_denial("tcp", PolicyNotReady::Draining);
                    return TrafficPolicy::NotReady(PolicyNotReady::Draining);
                }
                PolicyLifecycle::Stopped => {
                    if pending_waiter.take().is_some() {
                        record_tcp_pending_wait("stopped", started_at);
                    }
                    record_not_ready_denial("tcp", PolicyNotReady::Stopped);
                    return TrafficPolicy::NotReady(PolicyNotReady::Stopped);
                }
            }
        }
    }

    pub async fn udp_policy(&self) -> TrafficPolicy {
        let inner = self.state_rx.borrow().clone();
        let snapshot = self.snapshot_from_inner(&inner);
        let reason = match &inner.lifecycle {
            PolicyLifecycle::Ready(installed) => {
                // One live gate read per datagram, taken by the snapshot.
                if snapshot.interception_readiness.blocks_admission() {
                    // UDP has no connection to hold: a datagram either goes out
                    // inspected or not at all. Deny per datagram.
                    record_not_ready_denial("udp", PolicyNotReady::InterceptionNotReady);
                    return TrafficPolicy::NotReady(PolicyNotReady::InterceptionNotReady);
                }
                return TrafficPolicy::Ready(installed.policy.clone());
            }
            PolicyLifecycle::Pending { .. } => PolicyNotReady::PendingPolicy,
            PolicyLifecycle::NoIdentity => PolicyNotReady::NoIdentity,
            PolicyLifecycle::PolicyError(_) => PolicyNotReady::PolicyError,
            PolicyLifecycle::Draining { .. } => PolicyNotReady::Draining,
            PolicyLifecycle::Stopped => PolicyNotReady::Stopped,
        };
        debug!(target: "pplx_space_net_gateway::state", state = ?snapshot.mode, "UDP dropped while network policy is not ready");
        record_not_ready_denial("udp", reason);
        TrafficPolicy::NotReady(reason)
    }

    pub async fn set_draining(&self, reason: NetworkStateReason) {
        self.update_inner(|inner| {
            let previous = inner.lifecycle.installed().cloned();
            set_lifecycle(inner, PolicyLifecycle::Draining { previous }, reason);
            inner.diagnostics.current_retry_backoff_ms = 0;
        })
        .await;
    }

    pub async fn set_stopped(&self, reason: NetworkStateReason) {
        self.update_inner(|inner| {
            if let Some(installed) = inner.lifecycle.installed() {
                info!(target: "pplx_space_net_gateway::state",
                    cleared_policy_ref = %installed.reference,
                    reason = reason.as_str(),
                    "net-gateway policy cleared"
                );
            }
            set_lifecycle(inner, PolicyLifecycle::Stopped, reason);
            inner.diagnostics.current_retry_backoff_ms = 0;
        })
        .await;
    }

    fn start_tcp_pending_waiter(&self) -> TcpPendingWaiterGuard {
        self.pending_tcp_connections.fetch_add(1, Ordering::Relaxed);
        TcpPendingWaiterGuard {
            pending_tcp_connections: self.pending_tcp_connections.clone(),
            active: true,
        }
    }

    fn snapshot_from_inner(&self, inner: &NetworkStateInner) -> NetworkStateSnapshot {
        let policy = inner
            .lifecycle
            .installed()
            .map(|installed| installed.policy.clone());
        NetworkStateSnapshot {
            mode: inner.lifecycle.mode(),
            installed_policy_ref: inner
                .diagnostics
                .installed_policy_ref
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            last_error: inner
                .lifecycle
                .failure()
                .map(ToString::to_string)
                .unwrap_or_default(),
            last_error_kind: inner.lifecycle.error_kind(),
            last_hydration_latency_ms: inner.diagnostics.last_hydration_latency_ms,
            last_transition_unix_ms: inner.diagnostics.last_transition_unix_ms,
            pending_tcp_connections: self.pending_tcp_connections.load(Ordering::Relaxed),
            current_retry_backoff_ms: inner.diagnostics.current_retry_backoff_ms,
            consecutive_hydration_failures: inner.diagnostics.consecutive_hydration_failures,
            // Evaluated live on every read; the gate is not part of the
            // watch channel. Policy readiness is independent of CA readiness.
            interception_readiness: self.interception_readiness_for(policy.as_deref()),
            policy,
        }
    }
}

fn set_lifecycle(
    inner: &mut NetworkStateInner,
    lifecycle: PolicyLifecycle,
    reason: NetworkStateReason,
) {
    let from = inner.lifecycle.mode();
    let to = lifecycle.mode();
    if from != to {
        pplx_space_metrics::gateway::network_state_transition(
            from.as_str(),
            to.as_str(),
            reason.as_str(),
        );
        inner.diagnostics.last_transition_unix_ms = unix_time_ms();
    }
    inner.lifecycle = lifecycle;
}

/// The bounded, non-sensitive shape of a policy for the hydration log: the
/// action label plus per-class destination counts. Deliberately does NOT render
/// the destination strings — policy destinations are tenant-authored and a
/// hostname can carry arbitrary encoded data, so logging them on every
/// hydration would be an ungated log leak.
fn policy_log_summary(
    policy: &pplx_space_net_common::NetworkPolicyV1,
) -> (
    &'static str,
    pplx_space_net_common::EgressDestinationCounts,
    pplx_space_net_common::EgressDestinationCounts,
) {
    (
        match policy.default_action() {
            pplx_space_net_common::EgressDefaultAction::Allow => "default_allow",
            pplx_space_net_common::EgressDefaultAction::Deny => "default_deny",
        },
        policy.egress.allow_counts(),
        policy.egress.deny_counts(),
    )
}

fn record_hydration_failures(active_kind: NetworkPolicyErrorKind, active_failures: u64) {
    for kind in [
        NetworkPolicyErrorKind::None,
        NetworkPolicyErrorKind::LoaderError,
        NetworkPolicyErrorKind::Timeout,
        NetworkPolicyErrorKind::PolicyRefMismatch,
        NetworkPolicyErrorKind::GatewayStopping,
    ] {
        let failures = if kind == active_kind {
            active_failures
        } else {
            0
        };
        pplx_space_metrics::gateway::policy_hydration_consecutive_failures(kind.as_str(), failures);
    }
}

async fn wait_for_shutdown(duration: Duration, shutdown_token: &CancellationToken) -> bool {
    tokio::select! {
        _ = shutdown_token.cancelled() => true,
        _ = tokio::time::sleep(duration) => false,
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn record_tcp_pending_wait(result: &str, started_at: Instant) {
    pplx_space_metrics::gateway::pending_tcp_wait(result);
    pplx_space_metrics::gateway::pending_tcp_wait_ms(
        result,
        started_at.elapsed().as_millis() as f64,
    );
}

fn record_pending_tcp_slow_wait(snapshot: &NetworkStateSnapshot, started_at: Instant) {
    pplx_space_metrics::gateway::pending_tcp_slow_wait(
        effective_network_policy_ref_authority_label(&snapshot.installed_policy_ref),
    );
    let wait_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Only the hydration wait reaches here; unavailable interception
    // dependencies deny the connection instead of holding it. Carry the
    // readiness anyway so a responder is not sent hunting for a CA problem
    // that this wait is not evidence of.
    warn!(target: "pplx_space_net_gateway::state",
        wait_ms,
        state = snapshot.mode.as_str(),
        interception_readiness = snapshot.interception_readiness.as_str(),
        "TCP connection waited for network policy hydration"
    );
}

fn record_not_ready_denial(protocol: &str, reason: PolicyNotReady) {
    match reason {
        PolicyNotReady::NoIdentity => pplx_space_metrics::gateway::no_identity_denied(protocol),
        PolicyNotReady::Draining | PolicyNotReady::Stopped => {
            pplx_space_metrics::gateway::draining_closed(protocol)
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;

/// Cancels reserved-DNS work that began before a network-policy install.
///
/// Adapters take a child token before reading the admitted policy. The policy
/// installation cancels that token before publishing the replacement policy,
/// so the guest retries the transaction against the new policy instead of
/// receiving a response authorized by the old one.
#[derive(Debug)]
pub(super) struct ReservedDnsCancellation {
    current: parking_lot::Mutex<CancellationToken>,
}

impl Default for ReservedDnsCancellation {
    fn default() -> Self {
        Self {
            current: parking_lot::Mutex::new(CancellationToken::new()),
        }
    }
}

impl ReservedDnsCancellation {
    pub(super) fn operation_token(&self) -> CancellationToken {
        self.current.lock().child_token()
    }

    fn cancel_in_flight(&self) {
        let mut current = self.current.lock();
        current.cancel();
        *current = CancellationToken::new();
    }
}

impl Coordinator {
    fn reconcile_policy(&self, policy: &NetworkPolicyV1) {
        self.reserved_dns_cancellation.cancel_in_flight();
        self.dns_allow_cache.retain_relevant_to_policy(policy);
        let cancelled = self.tcp_connections.cancel_where(|admission| {
            !super::tcp::revalidate_tcp(admission, policy, &self.dns_allow_cache)
        });
        if cancelled > 0 {
            info!(target: "pplx_space_net_gateway::state::egress",
                dns_attributed_cancelled_tcp_connections = cancelled,
                "reconciled TCP connections for installed network policy"
            );
        }
    }
}

/// Initial lifecycle selected by the gateway's validated configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitialPolicyState {
    NoIdentity,
    PendingPolicy,
}

pub type TrafficGate = Arc<dyn Fn() -> bool + Send + Sync>;

/// Constructs one policy, attribution cache, connection registry and DNS epoch.
#[derive(Clone)]
pub struct GatewayNetwork {
    pub(super) coordinator: Arc<Coordinator>,
}

impl GatewayNetwork {
    pub fn new(initial: InitialPolicyState, readiness: Option<TrafficGate>) -> Self {
        Self {
            coordinator: Arc::new(Coordinator::new(initial, readiness)),
        }
    }

    pub fn egress(&self) -> Egress {
        Egress {
            coordinator: self.coordinator.clone(),
        }
    }

    pub fn control(&self) -> PolicyControl {
        PolicyControl {
            coordinator: self.coordinator.clone(),
        }
    }

    pub fn policies(&self) -> PolicyView {
        self.coordinator.policies()
    }
}

#[derive(Clone)]
pub struct Egress {
    pub(super) coordinator: Arc<Coordinator>,
}

/// Mutations and diagnostics for the gateway's installed policy.
#[derive(Clone)]
pub struct PolicyControl {
    pub(super) coordinator: Arc<Coordinator>,
}

impl PolicyControl {
    pub fn policies(&self) -> PolicyView {
        self.coordinator.policies()
    }

    pub async fn snapshot(&self) -> NetworkStateSnapshot {
        self.coordinator.snapshot().await
    }

    pub fn interception_readiness_observer(&self) -> ReadinessObserver {
        self.coordinator.interception_readiness_observer()
    }

    pub fn spawn_hydration(
        &self,
        loader: PolicyLoader,
        shutdown: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        self.coordinator.spawn_hydration(loader, shutdown)
    }

    pub async fn refresh(
        &self,
        loader: PolicyLoader,
        expected: Option<EffectiveNetworkPolicyRef>,
        reason: NetworkStateReason,
    ) -> NetworkStateSnapshot {
        self.coordinator.refresh(loader, expected, reason).await
    }

    pub async fn set_no_identity(&self, reason: NetworkStateReason) -> NetworkStateSnapshot {
        self.coordinator.set_no_identity(reason).await
    }

    pub async fn set_draining(&self, reason: NetworkStateReason) {
        self.coordinator.set_draining(reason).await;
    }

    pub async fn set_stopped(&self, reason: NetworkStateReason) {
        self.coordinator.set_stopped(reason).await;
    }
}

impl Coordinator {
    fn policies(&self) -> PolicyView {
        PolicyView {
            state_rx: self.state_rx.clone(),
        }
    }
}
