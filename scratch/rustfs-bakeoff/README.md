# RustFS vs versitygw bake-off

Measurement tools for deciding whether RustFS can be the Kind harness's
object store with credential-manager's storage exchange on. They are not
repository tests and run nothing in CI. All data is synthetic and every
credential is a local-dev constant. The results and the verdict are in
[REPORT.md](REPORT.md).

## Tools

| File | What it does |
|---|---|
| `stores.sh` | Runs either store in Docker with the same settings and the Kind chart's limits (1 CPU, 4 GiB), prints start → ready, creates RustFS's vend identity and policy (`vend`), finds a store's host-side veth (`veth`) and caps its ingress with tc (`throttle`). |
| `probe/` | A standalone Cargo package (its own workspace, not a member of the repository's) that drives SPACE's object-store client, `crates/libs/object-store`, and uses the AWS SDK only for requests that client never makes. One subcommand per criterion, each printing a Markdown table. |
| `perf.sh` | Criterion D for one store: image size, start → ready, idle and peak memory and CPU, first write to a new bucket, and three 36 × 128 MiB upload bursts with zero-progress sampling. |
| `results/` | The raw tables each criterion produced in this bake-off. |

## Build the probe

```bash
cd scratch/rustfs-bakeoff/probe
cp ../../../Cargo.lock .          # resolve the repository's dependency versions
export CARGO_TARGET_DIR=/var/tmp/rustfs-bakeoff-target   # any roomy directory
cargo build --release              # needs protoc, as the workspace does
PROBE=$CARGO_TARGET_DIR/release/rustfs-bakeoff-probe
```

## Run each criterion

Every probe subcommand takes `--endpoint`, `--access-key` and `--secret-key`
(or `BAKEOFF_ENDPOINT`, `BAKEOFF_ACCESS_KEY`, `BAKEOFF_SECRET_KEY`), so it runs
against any S3 endpoint. `stores.sh env <store>` prints those for a local
store.

```bash
cd scratch/rustfs-bakeoff
./stores.sh up rustfs && ./stores.sh vend rustfs
./stores.sh up versitygw
export BAKEOFF_SCRATCH=/var/tmp/rustfs-bakeoff-scratch   # synthetic files go here

for s in rustfs versitygw; do
  eval "$(./stores.sh env $s)"
  $PROBE --label $s behaviours            # A
  $PROBE --label $s digest                # B1
  $PROBE --label $s exchange              # C (versitygw: no STS)
done

# B2: cap ingress so a 200 MiB upload takes ~7 s, then freeze mid-upload.
./stores.sh throttle rustfs 240mbit
eval "$(./stores.sh env rustfs)"
$PROBE --label rustfs freeze --container bakeoff-rustfs   # 7, 17, 25 s; single PUT and 4-part stream

# C, expiry: the shortest session the store issues, waited out (~16 min for RustFS).
$PROBE --label rustfs expiry

# D: one store at a time, on a fresh container.
PROBE=$PROBE ./perf.sh rustfs
PROBE=$PROBE ./perf.sh versitygw

./stores.sh down rustfs; ./stores.sh down versitygw
```

`freeze` needs the Docker CLI (it runs `docker pause`/`unpause` and reads
`docker logs` for the store's own record of each attempt). `throttle` and the
burst's zero-progress sampling need a host whose network namespace holds the
Docker bridge, as a devbox's does.
