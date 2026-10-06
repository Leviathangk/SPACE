# Net-gateway module boundaries

Status: proposed target; migration in progress

The per-sandbox network gateway (`crates/libs/net/gateway/server`) enforces a
sandbox's network policy, answers its reserved DNS, optionally inspects TLS,
and carries ServicePort ingress. Today those concerns share one state object
and are wired together with callbacks in `run.rs`. This document defines the
module boundaries the gateway should converge on, the rules that keep them
clean, and how to move toward them incrementally while code is also being
deleted.

It describes structure, not new behavior. Unless a step says otherwise,
refactoring toward it must preserve the existing enforcement contract.

## Why

Observed coupling in the current tree:

- **Callback wiring.** `run()` builds about a dozen shared handles and joins
  them with an install hook and a policy-invalidation hook. Which components
  react to a policy change is visible only by reading `run()`.
  `TcpProxyConfig` takes 12 handles.
- **Global interception gate.** `GatewayNetworkState::with_traffic_gate` is
  wired to `GatewayAuthority::is_ready`. With `tls_interception = On` and the
  authority not ready, every new TCP flow and UDP datagram is refused,
  including flows that would be exempt or relayed. The gate also forces a
  cycle workaround in `interception_readiness_observer`.
- **Identity owned by ingress.** `GatewayAuthority` holds a
  `ServicePortIngressState` to read route and execution generations, so
  certificate code depends on the ingress module.
- **Secrets in shared state.** `NetworkStateInner` holds resolved credential
  values next to the policy, and TCP, UDP, DNS and control all read it.
- **Distributed verdict.** The final decision for a flow is assembled from
  `tcp_proxy/admission.rs`, `tls_interception.rs` (`before_relay`) and
  `state.rs` readiness. `TcpProxy::handle` re-runs `validate_admission_policy`
  after the async interception step.
- **Mixed state.** Lifecycle mode, policy content, credentials and hydration
  metrics share `NetworkStateInner`. `wait_for_tcp_policy` repeats a
  near-identical arm per lifecycle state.

## Modules

Each module owns its state, exposes a narrow interface, and depends only on
modules above it in this list. Signatures are illustrative; names may change
during migration.

### `identity`: who this gateway serves

Owns the sandbox id, route, execution and activation generations, and route
verification keys. Depends on nothing.

```rust
pub struct SandboxIdentity { /* sandbox_id, route_generation, execution_generation,
                                activation_attempt_id, node, substrate */ }

pub struct Identity { /* watch::Sender<Arc<SandboxIdentity>>, verification keys */ }
impl Identity {
    pub fn current(&self) -> Option<Arc<SandboxIdentity>>;
    pub fn watch(&self) -> watch::Receiver<Option<Arc<SandboxIdentity>>>;
    pub fn verify_route_token(&self, token: &RouteToken) -> Result<(), RouteTokenError>;
}
```

Moves from: identity fields of `ingress::ServicePortIngressState`,
`route_verification.rs`.

### `policy`: what is allowed right now

Owns the lifecycle state machine, hydration from host-control, and the
current immutable `PolicyGeneration`. Holds credential *references*, never
credential values. Depends on `identity`.

```rust
pub struct PolicyGeneration {
    pub id: GenerationId,              // replaces Arc pointer identity
    pub policy_ref: EffectiveNetworkPolicyRef,
    pub policy: NetworkPolicyV1,
    pub matchers: CompiledMatchers,    // allow/deny/exemption selectors
    pub credential_rules: Vec<NetworkCredentialRuleV1>,
}

pub enum Lifecycle { PendingPolicy, Ready, NoIdentity, PolicyError, Draining, Stopped }

pub struct Policy { /* lifecycle + watch of Arc<PolicyGeneration> */ }
impl Policy {
    pub fn admission(&self) -> Result<Arc<PolicyGeneration>, NotReady>;
    pub async fn wait_admission(&self, deadline: Instant) -> Result<Arc<PolicyGeneration>, NotReady>;
    pub fn subscribe(&self) -> watch::Receiver<Arc<PolicyGeneration>>;
    pub async fn refresh(&self, expected: Option<EffectiveNetworkPolicyRef>) -> PolicySnapshot;
    pub fn set_draining(&self); pub fn set_stopped(&self);
}
```

`NotReady` maps one-to-one from `Lifecycle`; one function replaces the
per-state arms. Moves from: `state.rs`, hydration in `run.rs`.

### `dns_evidence`: which addresses a domain resolved to

Owns the reserved resolvers and the DNS attribution cache. Only answers the
gateway obtained itself become evidence. Reconciles its cache when a new
generation arrives. Depends on `policy`.

```rust
pub struct DnsEvidence { /* reserved resolver core, attribution cache */ }
impl DnsEvidence {
    pub async fn answer(&self, gen: &PolicyGeneration, query: DnsQuery) -> DnsAnswer;
    pub fn evidence_for(&self, ip: IpAddr) -> Evidence; // observed names, expiry
    fn on_generation(&self, gen: &PolicyGeneration);    // prune, cancel in-flight
}
```

Moves from: `dns.rs`, `reserved_dns/`, the DNS half of the install hook.

### `decision`: the verdict for a flow

A pure function. No I/O, no async, no locks. Every egress rule lives here:
platform hard denies, explicit denies, allows, DNS-bound domain admission,
interception mode, exemptions, and any protocol-specific fail-closed rule
(for example UDP while interception is required). Depends on types only.

```rust
pub struct Flow { pub proto: Proto, pub dst: SocketAddr,
                  pub sni: Option<DomainName>, pub host: Option<DomainName>,
                  pub client_hello: ClientHelloScan }

pub enum Decision {
    Deny(DenyReason),
    Relay,                                   // admitted, no interception configured
    Exempt,                                  // admitted, interception on but exempt
    Intercept { credential_rules: Vec<RuleId> },
}

pub fn decide(gen: &PolicyGeneration, flow: &Flow, evidence: &Evidence) -> Decision;
```

Re-validation after an await becomes "is `gen.id` still current", followed by
`decide` again only when it is not. Moves from: `tcp_proxy/admission.rs`,
the selection logic in `tls_interception.rs`, and the egress helpers in
`net-common`.

### `egress`: moving outbound bytes

Owns the TPROXY TCP listener and UDP socket, non-consuming peek to build a
`Flow`, plain relay, and flow tracking keyed by generation. On a new
generation it re-decides live flows and closes those now denied. Depends on
`policy`, `dns_evidence`, `decision`, `Interceptor`, `limits`.

```rust
pub struct Egress<I: Interceptor> { /* listeners, trackers, deps */ }
impl<I: Interceptor> Egress<I> {
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), EgressError>;
}
// per TCP flow: accept -> peek -> decide -> Relay | Exempt -> relay
//                                       -> Intercept      -> interceptor.serve(..)
```

Moves from: `tcp_proxy.rs` minus TLS/HTTP legs, `tcp_proxy/{relay, tracker,
accounting, admission}`, `udp_proxy.rs`, `sniff.rs`, `tls_sni.rs`.

### `interception`: TLS and HTTP for flows that require it

The only module that holds TLS key material or credential values. Its
readiness affects only flows whose decision is `Intercept`. Depends on
`identity`, `policy`.

```rust
pub trait Interceptor: Send + Sync + 'static {
    fn readiness(&self) -> InterceptionReadiness;
    async fn serve(&self, conn: AdmittedConn, rules: &[RuleId],
                   gen: Arc<PolicyGeneration>) -> FlowOutcome;
}

pub struct NoInterception;   // policy needs no interception: serve() is unreachable
pub struct TlsInterceptor { /* authority, cert cache, credential store,
                               upstream TLS, HTTP legs, header sanitizer */ }
```

Moves from: `certificates/`, `tls_interception.rs`, `tcp_proxy/{tls,
tls_http, http_host, routing_headers}`, credential snapshot in `state.rs`.

### `ingress`: inbound ServicePort streams

Verifies route tokens against `identity`, opens guest connections, and pumps
frames. Depends on `identity`, `policy` (lifecycle only), `limits`.

```rust
impl Ingress {
    pub async fn open(&self, start: ServicePortIngressStart, meta: &MetadataMap)
        -> Result<IngressStream, Status>;
}
```

Moves from: `ingress/` (minus identity).

### `control`: gRPC adapter

Implements `NetGatewayControl` by calling the modules above. Owns no state
and contains no policy logic. Moves from: `control.rs`.

### Support: `observability` and `limits`

- `observability`: stats, sandbox events, debug state and known-egress
  diagnostics. Modules write to it; no decision reads from it.
- `limits`: file-descriptor and concurrency budgets as owned permits.

Moves from: `stats.rs`, `debug.rs`, `known_egress.rs`, `resource_budget.rs`.

## Composition

`run()` becomes the only place that names concrete types:

```text
identity ─► policy ─► dns_evidence
                 └──► decision (pure)
interceptor = TlsInterceptor | NoInterception   (chosen from policy)
egress(policy, dns_evidence, interceptor, limits, observability)
ingress(identity, policy, limits, observability)
control(identity, policy, egress handle, ingress, interceptor)
```

Target layout under `src/`:

```text
core/{identity, policy, lifecycle}
evidence/dns
egress/{decide, tcp, udp}
interception/{tls, http, certificates, credentials}
ingress/
control/
support/{observability, limits}
run.rs                      # composition root
```

## Rules

1. Dependencies point one way. Nothing depends on `control` or `egress`;
   `decision` has no runtime dependencies.
2. All allow, deny, exempt and intercept logic lives in `decision`. A policy
   check elsewhere is a bug to move, not a pattern to copy.
3. Only `interception` holds TLS keys or credential values.
4. Policy changes propagate as generations. Components subscribe and
   reconcile their own state; no install or invalidation callbacks.
5. Interception readiness is scoped to `Intercept` decisions, never a
   sandbox-wide traffic gate.
6. Only `control` speaks gRPC. Only `run.rs` wires modules together. Modules
   take narrow dependencies, not handle bundles.

## Migrating incrementally

Code is being deleted from the gateway at the same time, so the migration is
opportunistic rather than a single rewrite.

**When deleting code:** remove it outright rather than moving it first. If a
deletion leaves a module boundary obviously simpler, note it in the tracker
below.

**When touching code anyway:** leave it closer to the target, within the
change's scope.

- New or changed egress rules go in `decide` (create `egress/decide.rs` the
  first time one is needed), with table tests.
- New consumers of policy changes subscribe to the generation watch; do not
  add another hook closure in `run.rs`.
- Do not add new readers of credential values or of `traffic_gate`.
- Do not add identity reads through `ServicePortIngressState`.
- Prefer passing a narrow trait or struct over adding a field to
  `TcpProxyConfig` or `UdpProxyConfig`.

**Dedicated steps**, each independently mergeable. Steps 1 to 3 and 5 change
no behavior; step 4 changes behavior.

| # | Step | Behavior change | Status |
|---|---|---|---|
| 1 | Extract `Decision` and `decide()`; route TCP and UDP through it; table tests | No | Not started |
| 2 | Add `PolicyGeneration` with explicit id and subscribers; delete install and invalidation hooks; split `Lifecycle` from policy content | No | Not started |
| 3 | Extract `identity`; ingress and certificates depend on it | No | Not started |
| 4 | Put interception behind `Interceptor` with `NoInterception`; scope readiness to `Intercept`; remove `traffic_gate`. Keep any intended UDP fail-closed rule explicit in `decide` | Yes, behind a flag defaulting to current behavior | Not started |
| 5 | Move credential values into `interception`; core never holds secrets | No | Not started |
| 6 | Move files into the target layout | No | Not started |

Step 6 is last so earlier diffs stay reviewable. Splitting `interception`
into a separate process is out of scope.

## Validation

Steps without a behavior change keep existing gateway tests and benches
passing unchanged. Step 1 adds table tests covering every `DenyReason` and
each of `Relay`, `Exempt` and `Intercept`. Step 4 adds tests showing that an
unready authority refuses only `Intercept` flows when the flag is enabled and
preserves current refusals when it is disabled.
