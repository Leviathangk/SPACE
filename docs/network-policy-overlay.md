# Optional network policy overlay

[Documentation](../README.md)

The ECW create header `X-Space-Network-Policy-Overlay: true` selects a single
operator-configured network policy overlay. Only authenticated federated creates
can select it. Creates without the header retain their normal policy.

Set `SPACE_NETWORK_POLICY_OVERLAY` on each API process to a JSON object:

```json
{
  "egress_allowances": [{"domain":"agent.example.test"}, {"cidr":"192.0.2.0/24"}],
  "tls_interception_exemptions": [{"domain":"agent.example.test"}]
}
```

These are synthetic destinations; supply the approved list for your deployment.
Both lists accept `ip`, `cidr`, `domain`, and `wildcard_domain` destinations.
There are no built-in endpoints. Fields default to empty lists; `{}` disables
all additions. Invalid JSON, unknown fields, and invalid destinations fail startup.
Configuration is parsed once at startup. Without generic or legacy configuration,
no overlay is available.
A missing configuration or invalid header leaves ordinary creation intact;
authentication, owner/grant validation, and provisioning errors still apply.
The header accepts `true` case-insensitively with surrounding whitespace removed.

Composition uses the request's network policy when supplied, otherwise the
resolved template policy. It adds and deduplicates each list independently:

- Egress allowances permit traffic, subject to explicit denies and platform hard
  denies. They do not exempt traffic from TLS interception.
- TLS exemptions bypass TLS termination, HTTP inspection, and credential
  injection. They do not grant egress permission.

Default-deny policies with hostname allowances receive the shared ECW DNS
resolver allowances. Those resolvers are not automatically exempted from TLS
interception. Destinations do not constrain transport or port.

Hostname exemptions require a live observed DNS binding at admission. IP and
CIDR exemptions apply regardless of SNI. The composed ordinary network policy is
persisted for cold and pooled creates. Wake and restore reuse the saved policy;
changing configuration affects new creates, not existing sandboxes.

`space.sandbox.network_policy_overlay_create_admission` records selection with
`outcome:ok|failed` and `reason:ready|invalid_request|unconfigured`. It describes
policy preparation, not the health of a guest workload.

Before configuring nonempty TLS exemptions, upgrade every policy reader and
running worker gateway. Follow the [TLS exemption rollout and rollback
steps](architecture.md#tls-interception-exemptions). The deployment-wide
`SPACE_TLS_INTERCEPTION_EXEMPTIONS` setting remains independent: unlike this
overlay, it does not require per-create opt-in.

## Migrating existing deployments

Binary upgrades can precede operator configuration and caller updates. If
`SPACE_NETWORK_POLICY_OVERLAY` is absent, the API retains the legacy
`SPACE_FALCON_US2_EGRESS_DESTINATIONS` configuration and
`SPACE_FALCON_TLS_INTERCEPTION_EXEMPTIONS_ENABLED` activation flag. Legacy lists
retain their existing validation: nonempty exact IP/domain destinations, and a
boolean activation flag defaulting to false. There are no built-in destinations.

The generic configuration takes precedence when present, including `{}` to
stop additions; invalid generic configuration fails startup rather than falling
back. ECW continues accepting `X-Space-Falcon: us2` for authenticated federated
creates, selecting whichever configuration is active. An explicit generic header
takes precedence over the legacy header, including `false`. The legacy header
continues to have no effect on native creates.

Upgrade binaries first, migrate operator configuration second, then migrate
callers to the generic header. The historical
`space.sandbox.falcon_create_admission` counter continues receiving overlay
outcomes during migration. Remove compatibility inputs and the legacy counter
only after all deployments and callers have migrated.
