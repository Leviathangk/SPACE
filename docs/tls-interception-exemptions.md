# Deployment-configured TLS interception exemptions

TLS interception currently terminates TLS and parses the decrypted stream as
HTTP. Non-HTTP TLS protocols (for example TURN-over-TLS, or Chrome's push
messaging connection to `mtalk.google.com` on port 5228) can therefore receive
an HTTP 400 response and a closed connection. Deployment-configured exemptions
are a stopgap that leaves selected TLS streams encrypted end-to-end, not
protocol-aware interception.

The gateway does not fall back to relaying decrypted bytes that it cannot parse
as HTTP. The guest chooses those bytes, so such a fallback would let any guest
skip request inspection on an allowed host. Choosing which destinations skip
inspection is an operator decision, made by listing them here.

Set `SPACE_TLS_INTERCEPTION_EXEMPTIONS` to the same JSON array on both the API
service and the cluster-controller process that executes queued operations:

```json
[{"domain":"relay.example.test"},{"wildcard_domain":"*.relay.example.test"}]
```

Both processes validate this configuration at initialization, including
cluster-controller standbys before leader election. Unset means disabled;
empty, malformed, invalid-domain, and non-hostname destinations fail startup.
There is no built-in destination list. Wildcards match leading-label suffixes,
not arbitrary patterns, and do not match the suffix's apex domain. Destinations
have no port: an exempted hostname is exempt on every port.

For sandbox creation and network-policy updates, configured destinations are
appended only when the effective policy has `tls_interception: "on"`. Existing
exemptions are preserved and duplicate additions are skipped. An inherited
template policy becomes a sandbox override only when composition adds an
exemption. The API composes E2B-translated creates; the cluster-controller
composes queued native/admin creates (including admitted warm-pool claims),
policy updates and resets to the template default, and disk fork targets.
Restoring an existing sandbox retains its persisted policy; this setting does
not retroactively rewrite existing sandboxes or template defaults. Policy
updates can apply it to existing sandboxes.

This setting never adds egress allows (including DNS resolver allows) or changes
denies. Exemption does not grant network access. Hostname exemption matching
requires gateway DNS attribution: the guest must resolve via the reserved
resolvers `8.8.8.8` or `1.1.1.1`, and its existing policy must permit the required
DNS and destination traffic. A custom DNS client still qualifies when it sends
queries through those resolvers; SNI alone is not attribution. DNS-over-HTTPS,
other resolvers, expired attribution, or cached addresses without observed DNS
can leave interception enabled.

Browsers can switch to DNS-over-HTTPS on their own. With `8.8.8.8` and
`1.1.1.1` as its system resolvers, an unmanaged Chrome can move its lookups to
those providers' DNS-over-HTTPS endpoints a few seconds after startup. The
gateway then no longer observes the lookups that hostname exemptions rely on.
Setting Chrome's `DnsOverHttpsMode` policy to `"off"` keeps its lookups on the
reserved resolvers.

Before enabling, all policy readers and worker gateways must support
`tls_interception_exemptions`. Removing the environment variable from both
processes and rolling their replicas stops future composition but does not
remove exemptions already persisted in sandbox overrides; remove those
explicitly to roll back existing sandboxes.
