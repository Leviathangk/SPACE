# Prompt for a local devbox coding agent

Copy everything below into the devbox agent session after checking out the draft
certification branch `psi/otlp-observability-certification` in `perplexityai/SPACE`.

---

Run the SPACE OTLP observability certification end to end on this devbox and
return its evidence. Read `AGENTS.md`, `infra/space/kind/AGENTS.md`, and
`docs/observability-certification.md` first. Use the existing harness; do not
modify application code, charts, metric expectations, xfails, or guest isolation
just to obtain a pass. Only fix harness bugs if a failed check demonstrably comes
from the harness; preserve the failing artifacts and explain any fix separately.
Do not modify other PRs, request reviewers, merge, or enable Datadog SaaS.

1. Verify this is the intended certification branch, record its commit and base,
   and verify Docker, Linux x86_64, `/dev/kvm`, btrfs/loop support, free RAM/disk,
   and documented tool versions. The default runtime is separately pinned to
   `a85c35f98cc9d6628184c1c561ba908fdbc6f0e6`; do not replace it with floating main.
   Fetch it if necessary. Refuse an existing developer `space-local` cluster;
   use a dedicated devbox rather than deleting or adopting someone else's work.
2. Run:

   ```bash
   python3 -m unittest discover -s scripts/tests -p test_observability_certification.py -v
   bash -n scripts/tests/observability/certification/workload.sh
   shellcheck scripts/tests/observability/certification/workload.sh
   ```

3. Choose a fresh artifacts path and run:

   ```bash
   cert_output="$HOME/space-certification-results/run-$(date -u +%Y%m%dT%H%M%SZ)"
   python3 scripts/tests/observability/certification/run.py run \
     --grafana --artifacts "$cert_output"
   cert_status=$?
   printf 'Certification exit status: %s\nArtifacts: %s\n' "$cert_status" "$cert_output"
   cat "$cert_output/summary.md"
   ```

   Allow the documented cold-build time. The runner owns its fresh Kind cluster
   and tears it down on ordinary exit. Never globally prune Docker or shared
   caches. If interrupted by SIGKILL/reboot, use the documented ownership-checked
   `teardown` command with the same state directory and a new artifacts path.
4. Inspect every failure and expected failure using the saved query responses,
   command logs, image IDs, source/chart revisions and timings. Distinguish product
   defects, documented gaps, missing prerequisites, and harness bugs. Missing
   traffic is not a pass, absent gauge data is not zero, and different trace IDs
   do not establish cross-service parenting. Do not hide query errors with xfail.
5. Guest service and filesystem identity-worker checks must remain explicit skips
   until an independently reviewed collection path exists. The healthy-workload
   requested-template-version qualification is also documented. Report these
   limitations even if the process exits 0. Use `--require-fix` only when testing
   revisions that should contain the named fixes; do not cherry-pick or edit
   unrelated chart branches automatically.
6. Verify teardown (or explicitly disclose any retained owned resources). Collect
   `summary.md`, `report.json`, expected-name snapshot, rendered charts, image
   inventory, relevant query JSON, and failing command logs. Keep raw output
   private: redact local session/attach tokens before sharing. Do not commit
   run artifacts. If a harness fix was necessary, record its patch, rerun the
   appropriate assertion checks, then repeat the certification into a new directory.
7. Return: branch/base and exact commits; tool versions; total and phase runtimes;
   pass/fail/xfail/skip counts and each unresolved check; runtime and chart image
   SHAs; a representative cross-service trace ID and parent/child services;
   per-producer coverage; builder exclusion and Datadog-default control results;
   artifact location; and teardown status. Explicitly state what was not run.
   Do not call the full platform certified while any expected gap remains.
