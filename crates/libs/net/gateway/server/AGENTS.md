# Net-gateway server

This crate is migrating toward the module boundaries in
[`docs/space/design/net-gateway-modules.md`](../../../../../docs/space/design/net-gateway-modules.md).
When changing code here, leave it closer to that target within the change's
scope:

- Put new or changed egress verdict logic in the pure `decide()` function, with
  table tests, not in the TCP, UDP or interception paths.
- React to policy changes by subscribing to the policy generation, not by
  adding hook closures in `run.rs`.
- Do not add readers of resolved credential values outside the interception
  code, or new closures over the certificate authority like `traffic_gate`.
  The sandbox-wide refusal while interception is required and not ready is
  intended; keep it in policy admission.
- Do not read sandbox identity through `ServicePortIngressState`.
- Prefer deleting dead code over moving it.

Update the step tracker in the design document when a migration step lands.
