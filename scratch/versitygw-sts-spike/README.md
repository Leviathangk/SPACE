# versitygw STS spike

Tools for deciding whether Kind can keep versitygw as its object store when
credential-manager's storage exchange is on, with a small Kind-only bridge
turning the authority's unchanged STS `AssumeRole` into versitygw's
`AssumeRoleWithWebIdentity`. They are not repository tests and run nothing in
CI. All data is synthetic and every credential is a local-dev constant or is
generated at bring-up. The results and the verdict are in
[REPORT.md](REPORT.md).

## Tools

| File | What it does |
|---|---|
| `stack.sh` | Runs versitygw v1.9.0's standalone IAM service and its S3 gateway (private API over a shared unix socket) and the bridge in Docker, then registers the OIDC provider, the vend role and a decoy role through the IAM API. |
| `bridge/` | The bridge prototype (Go, standard library only): `serve` verifies the AssumeRole's SigV4 signature, mints a short-lived token for the verified identity, calls `AssumeRoleWithWebIdentity`, answers `AssumeRoleResponse`, and serves the issuer's discovery document and JWKS over HTTPS; `keygen` makes the test CA, TLS leaf and signing keys; `mint` prints one token with chosen claims for the refusal checks. |
| `probe/` | The RustFS bake-off's probe (`scratch/rustfs-bakeoff/probe` on `ac/kind-exchange-rustfs-bakeoff`), extended: `--sts-mode` issues sessions through versitygw directly (`web-identity`), through the bridge with the SDK's STS client (`bridge`), or with the authority's own `AssumeRoleProvider` code (`provider`); `trust`, `bridge-auth`, `early` and `session` are new; `digest` and `burst` can write with session credentials. SPACE's object-store client makes every request it makes in production. |
| `burst.sh` | The bake-off's 36 × 128 MiB burst on the running stack with root keys, a vended session and an IAM user's keys, interleaved. |
| `results/` | The tables each run produced. |

## Run

On a Linux box whose network namespace holds the Docker bridge (the probe
reaches containers by IP, and the burst samples the gateway's veth):

```bash
cd scratch/versitygw-sts-spike
./stack.sh up                       # builds the bridge with go, needs the aws CLI

cd probe && cp ../../../Cargo.lock . && cd ..
export CARGO_TARGET_DIR=/var/tmp/versitygw-sts-spike-target   # any roomy directory
(cd probe && cargo build --release)  # needs protoc, as the workspace does
P=$CARGO_TARGET_DIR/release/versitygw-sts-spike-probe

export BAKEOFF_SCRATCH=/var/tmp/versitygw-sts-spike-scratch RUST_LOG=error
eval "$(./stack.sh env)"

# C: criterion C three ways.
$P --label "web-identity" exchange --sts-mode web-identity
$P --label "bridge, SDK client" exchange --sts-mode bridge
$P --label "bridge, authority client" exchange --sts-mode provider

# C, expiry: versitygw's shortest session is 900 s, so each takes ~16 min.
$P --label "web-identity" expiry --sts-mode web-identity &
$P --label "bridge, authority client" expiry --sts-mode provider &

# Trust, forged credentials, early refusals; then the bridge's own checks.
$P --label versitygw trust
$P --label "versitygw + bridge" bridge-auth --sts-mode provider
$P --label versitygw early --repeats 20   # how a refused 20 MiB PUT reaches the client

# B1 under a session the authority's client got through the bridge.
eval "$($P session --sts-mode provider --prefix spike/b1)"
$P --label "session" digest --bucket snapshots --key-prefix spike/b1
unset BAKEOFF_WRITER_ACCESS_KEY BAKEOFF_WRITER_SECRET_KEY BAKEOFF_WRITER_SESSION_TOKEN
wait

# D: the burst, static keys against session credentials.
PROBE=$P ./burst.sh

./stack.sh down
```
