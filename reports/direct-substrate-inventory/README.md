# Direct-substrate inventory (phase 0)

This is a read-only inventory of where the Direct-vs-Kubernetes substrate split
leaks into shared code, at commit `c565c8edcc29b8ccab926297738b54f1826c76e9`.
It is the baseline for the later phases: (1) an archcheck ratchet, (2) one name
for the substrate predicate in Rust and SQL, (3) capabilities behind
`SubstrateAdapter`, (4) tests that stop using Direct as a fixture, and (5)
Direct's contract.

[`inventory.csv`](inventory.csv) has one row per classified line. This README
explains how the rows were produced and what they show. No code changed.

**Vocabulary.** The control plane's Postgres has three `managed_by` columns, and
they must not be confused:

| column | values | meaning |
|---|---|---|
| `sandboxes.managed_by` | `'space'` \| `'k8s'` (NOT NULL, CHECK) | the substrate that owns the sandbox row, next to `public.sandboxes.lifecycle` |
| `nodes.managed_by` | `'space'` \| `'k8s'` (NOT NULL, CHECK) | the substrate that registered the node |
| `kubernetes_pool_configs.managed_by` | `'manual'` \| `'autoscaler'` | who owns a warm-pool config; **not the substrate** |

The control-plane lifecycle store is Postgres `public.sandboxes.lifecycle`. The
node-local store is sbx-mgr's SQLite `sandboxes.state`. Unless a row says
otherwise, "lifecycle write" here means a write to `public.sandboxes.lifecycle`
or to a related control-plane table.

## 1. Method

### Scope rule

- **Production Rust file:** archcheck's definition
  (`crates/tools/archcheck/README.md`, "What is checked"). That is a `.rs` file
  under `<member>/src/` of a workspace member in the root `Cargo.toml`, except
  `tests.rs`, `*_tests.rs` and anything below a `tests/` directory. Symlinks
  below `src/` are skipped. This gives 1186 files.
- **Test-only code inside those files is excluded** in two forms:
  - **Inline test-only items:** a `#[cfg(test)]` or `#[cfg(all(test, ..))]`
    attribute, through the end of the item it annotates. `#[cfg(any(test, ..))]`
    is kept, because it is not test-only.
  - **Test-only module files:** files whose own `mod x;` declaration is
    test-only, such as `#[cfg(test)] mod lifecycle_writer_test_support;`. There
    are 17 such files. archcheck's filename rule cannot see them.
- **Code lines only:** archcheck's code line, meaning not blank and not
  starting with `//` after trimming. `///` and `//!` doc lines are dropped.
  Block comments and string literals count.
- **Rows:**
  - outside `crates/libs/api/src/controllers/substrate/`: every code line that
    matches any search pattern;
  - inside that module: only the `managed_by` lines.
- **Out of scope**, raw `git grep -n` line counts only, not classified:
  - `ui/`: `managed_by` 27 lines; `'k8s'`/`'space'`/`"k8s"`/`"space"` literals 10 lines.
  - SQL migrations (`db/`, of which `db/postgres/20261002070625_baseline.up.sql` is the control plane's; `crates/libs/sbx/mgr/migrations/`): `managed_by` 11 + 0 lines; literals 8 + 0 lines.
  - `scripts/`: `managed_by` 4 lines; literals 36 lines.

### Search patterns (what was searched)

The inventory searches these Python regexes on each physical line. Their keys
appear in the scripts below.

| key | regex | why |
|---|---|---|
| `managed_by` | `managed_by` | every column, field, getter, bind and label |
| `MANAGED_BY_const` | `\bNODE_MANAGED_BY_(K8S\|SPACE)\b` | the Rust string constants in `common/core/src/types/node.rs` |
| `Substrate_variant` | `\bSubstrate::(Direct\|Kubernetes)\b` | matches on the api `Substrate` enum |
| `substrate_type` | `\b(DirectSubstrate\|KubernetesSubstrate)\b` | concrete substrate types outside the module |
| `is_pred` | `\b(is_direct\|is_kubernetes\|is_k8s\|is_space_managed\|is_k8s_managed)\b` | predicate helpers |
| `enum_variant` | `\b(NodeManagedBy\|SandboxSubstrateKind\|SubstrateKind\|TriggerSubstrate\|ConsoleSubstrate\|SubstrateSinkBackend)::\w+` | every enum that names a substrate |
| `lit_rust` | `"(k8s\|space\|kubernetes\|direct)"` | Rust string literals |
| `lit_sql` | `'(k8s\|space)'` | SQL string literals |
| `cfg_kubernetes` | `feature\s*=\s*"kubernetes"` | `cfg`/`cfg_attr` gates |
| `helper_fn` | `\b(routable_execution_or_non_k8s\|\w*k8s_finalization_sql\|SuspendedK8sFinalizationSql\|require_direct_substrate\|DirectUnsupported\|substrate_kind\|effective_substrate_kind\w*)\b` | helpers that encode a substrate in their name |

The other spellings came from an identifier sweep over production files outside
the module:
`xargs grep -ohE '\b[A-Za-z_]*(Substrate|substrate|ManagedBy|managed_by|Direct|K8s|k8s|Kubernetes)[A-Za-z_]*\b' | sort | uniq -c`.
Notes on what it found:

- **`is_direct()` / `is_kubernetes()` do not exist.** The only `is_direct` /
  `is_k8s` hits are an unrelated test helper and a local
  `existing_is_k8s` inside the module.
- **No `SubstrateKind` Rust type exists.** The proto enum
  `SandboxSubstrateKind` (`proto/net/gateway/v1/gateway.proto:265`) is the
  route-identity wire form.
- **Spellings that name the substrate in shared code:** `NodeManagedBy`,
  `SandboxSubstrateKind`, `TriggerSubstrate`, `ConsoleSubstrate`,
  `SubstrateSinkBackend`, `routable_execution_or_non_k8s`,
  `*_k8s_finalization_sql`, `require_direct_substrate` and `DirectUnsupported`.
- **Hits that are not the substrate** are kept as category `x`, so false
  positives stay visible:
  - the `k8s` cargo feature and the auth labels of Kubernetes service-account
    auth;
  - the `"direct"` route transport and port-bridge status;
  - the `"space"` namespace, RuntimeClass and nftables names;
  - `PoolConfigOwner`.

### Categories

`a`–`f` are the categories from the task. `x` is extra: the pattern matched,
but the line is not a substrate construct. Without `x`, the false-positive
cost of each pattern would be invisible.

| | category | phase |
|---|---|---|
| a | data, not a branch (columns, INSERT values, projections, fields) | leave |
| b | loop scoping: a query or reconciler that only applies to one substrate's rows | 2 (named predicate) |
| c | writer or projection gating: a lifecycle or related-table write, or a projection, that applies to one substrate | 2 (named predicate) |
| d | capability branch in Rust | 3 (capability) |
| e | metric, log or trace label | leave |
| f | node-side (sbx-mgr, gateway, node-api sim, node-ctl, route identity) | keep out of the ratchet |
| x | pattern hit that is not the substrate | leave |

For `b`/`c` rows, `capability` holds the predicate:

- `MANAGED_BY_K8S` / `MANAGED_BY_SPACE` / `NOT_MANAGED_BY_K8S`: on `sandboxes`;
- `NODE_MANAGED_BY_K8S` / `NODE_MANAGED_BY_SPACE` / `NODE_NOT_MANAGED_BY_SPACE`:
  on `nodes`;
- `MANAGED_BY_ACTIVE`: `managed_by = $n`, bound to
  `SubstrateAdapter::managed_by()`;
- `ANY_MANAGED_BY`: `IN ('space', 'k8s')`.

Which table a bare `managed_by` belongs to comes from the nearest preceding
`FROM`/`UPDATE`/`JOIN` (`nearest_table`). Whether a SQL line is `b` or `c`
comes from the verbs of the enclosing statement string, read in both
directions (`stmt`). This handles CTE `SELECT`s that feed an `UPDATE`.

### How every count was produced

Every number in this report comes from one script, `inventory.py`, pasted
below in full. It is not committed. Run it from the repository root:

```sh
python3 inventory.py csv       reports/direct-substrate-inventory/inventory.csv
python3 inventory.py reconcile
python3 inventory.py counts    reports/direct-substrate-inventory/inventory.csv
python3 inventory.py sqlform   reports/direct-substrate-inventory/inventory.csv
python3 inventory.py ratchet   reports/direct-substrate-inventory/inventory.csv
```

The rule tables in `inventory.py` (`rule(...)` calls) are the classification
decisions. Each one was made after reading the surrounding code. SQL predicates
not covered by a rule are classified mechanically by statement verb.

<details><summary><code>inventory.py</code> (click to expand)</summary>

```python
#!/usr/bin/env python3
"""Direct-substrate inventory (phase 0). Run from the repository root:

  python3 inventory.py csv reports/direct-substrate-inventory/inventory.csv
  python3 inventory.py reconcile
  python3 inventory.py counts  reports/direct-substrate-inventory/inventory.csv
  python3 inventory.py sqlform reports/direct-substrate-inventory/inventory.csv
  python3 inventory.py ratchet reports/direct-substrate-inventory/inventory.csv [-v]
"""
import csv, os, re, sys, tomllib
from collections import Counter, defaultdict

# ---- scope
def prod_files(root):
    members = tomllib.load(open(os.path.join(root, "Cargo.toml"), "rb"))["workspace"]["members"]
    out = set()
    for m in members:
        src = os.path.join(root, m, "src")
        if not os.path.isdir(src):
            continue
        stack = [src]
        while stack:
            d = stack.pop()
            for e in os.scandir(d):
                if e.is_symlink():
                    continue
                if e.is_dir():
                    if e.name != "tests":
                        stack.append(e.path)
                elif e.name.endswith(".rs") and e.name != "tests.rs" and not e.name.endswith("_tests.rs"):
                    out.add(os.path.relpath(e.path, root))
    return sorted(out)

CFG_TEST = re.compile(r'^\s*#\[cfg\((test|all\(test\b[^]]*)\)\]')

def _code_mask(text):
    """Return text with comments, strings and char literals blanked (newlines kept)."""
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i); j = n if j < 0 else j
            out.append(" " * (j - i)); i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j): depth += 1; j += 2
                elif text.startswith("*/", j): depth -= 1; j += 2
                else: j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j])); i = j
        elif c == "r" and re.match(r'r#*"', text[i:i+20]) and (i == 0 or not (text[i-1].isalnum() or text[i-1] == "_")):
            m = re.match(r'r(#*)"', text[i:]); hashes = m.group(1)
            j = text.find('"' + hashes, i + len(m.group(0)))
            j = n if j < 0 else j + 1 + len(hashes)
            out.append(re.sub(r"[^\n]", " ", text[i:j])); i = j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j])); i = j
        elif c == "'":
            m = re.match(r"'(\\.[^']*|[^\\'])'", text[i:i+12])
            if m:
                out.append(" " * len(m.group(0))); i += len(m.group(0))
            else:
                out.append(c); i += 1  # lifetime
        else:
            out.append(c); i += 1
    return "".join(out)

def cfg_test_lines(text):
    """Lines of test-only items: `#[cfg(test)]` / `#[cfg(all(test, ..))]`
    through the end of the item it annotates (the matching `}` of its first
    block, or its first top-level `;`/`,` if it has none: `mod tests;`, an
    enum variant, a field, a match arm). `any(test, ..)`
    is not test-only and is kept."""
    masked = _code_mask(text).split("\n")
    excluded, k = set(), 0
    while k < len(masked):
        if not CFG_TEST.match(masked[k]):
            k += 1
            continue
        rest = CFG_TEST.sub("", masked[k], count=1)
        depth, paren, started, j, done = 0, 0, False, k, False
        while j < len(masked) and not done:
            line = rest if j == k else masked[j]
            for ch in line:
                if ch in "([":
                    paren += 1
                elif ch in ")]":
                    paren -= 1
                elif ch == "{":
                    depth += 1; started = True
                elif ch == "}":
                    depth -= 1
                    if depth <= 0 and (started or depth < 0):
                        done = True; break
                elif ch in ";," and not started and paren == 0:
                    done = True; break
            if not done:
                j += 1
        excluded.update(range(k + 1, j + 2))
        k = j + 1
    return excluded

def code_lines(text):
    """(lineno, line) for archcheck code lines: non-blank, not starting with //."""
    text = text.lstrip("﻿")
    for i, l in enumerate(text.split("\n"), 1):
        s = l.strip()
        if s and not s.startswith("//"):
            yield i, l

MOD_DECL = re.compile(r'^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;')
PATH_ATTR = re.compile(r'#\[path\s*=\s*"([^"]+)"\]')

def _child_dir(f):
    """Directory holding the submodule files of module file f."""
    d, base = os.path.split(f)
    if base in ("mod.rs", "lib.rs", "main.rs"):
        return d
    return os.path.join(d, base[:-3])

def cfg_test_module_files(root, files):
    """Production files whose own `mod x;` declaration is test-only, plus
    everything below them: compiled only under cfg(test), though archcheck's
    filename rule counts them."""
    fileset = set(files)
    roots = set()
    for f in files:
        text = open(os.path.join(root, f), encoding="utf-8").read().lstrip("﻿")
        ex = cfg_test_lines(text)
        lines = text.split("\n")
        for i, l in enumerate(lines, 1):
            m = MOD_DECL.match(l)
            if not m or i not in ex:
                continue
            prev = lines[i - 2] if i >= 2 else ""
            p = PATH_ATTR.search(prev)
            if p:
                cands = [os.path.normpath(os.path.join(os.path.dirname(f), p.group(1)))]
            else:
                cd = _child_dir(f)
                cands = [os.path.join(cd, m.group(1) + ".rs"), os.path.join(cd, m.group(1), "mod.rs")]
            for c in cands:
                if c in fileset:
                    roots.add(c)
    out = set()
    for r in roots:
        sub = _child_dir(r) + "/"
        out.add(r)
        out.update(x for x in fileset if x.startswith(sub))
    return out

# ---- search patterns
# Inventory search patterns (Python re, matched per physical line).
PATTERNS = {
    "managed_by":        r"managed_by",
    "MANAGED_BY_const":  r"\bNODE_MANAGED_BY_(K8S|SPACE)\b",
    "Substrate_variant": r"\bSubstrate::(Direct|Kubernetes)\b",
    "substrate_type":    r"\b(DirectSubstrate|KubernetesSubstrate)\b",
    "is_pred":           r"\b(is_direct|is_kubernetes|is_k8s|is_space_managed|is_k8s_managed)\b",
    "enum_variant":      r"\b(NodeManagedBy|SandboxSubstrateKind|SubstrateKind|TriggerSubstrate|ConsoleSubstrate|SubstrateSinkBackend)::\w+",
    "lit_rust":          r"\"(k8s|space|kubernetes|direct)\"",
    "lit_sql":           r"'(k8s|space)'",
    "cfg_kubernetes":    r"feature\s*=\s*\"kubernetes\"",
    "helper_fn":         r"\b(routable_execution_or_non_k8s|\w*k8s_finalization_sql|SuspendedK8sFinalizationSql|require_direct_substrate|DirectUnsupported|substrate_kind|effective_substrate_kind\w*)\b",
}

# ---- statement context for SQL lines
ROOT = ""
VERB = re.compile(r"\b(SELECT|UPDATE|DELETE\s+FROM|INSERT\s+INTO|WITH|ON\s+CONFLICT|RETURNING|SET\s+lifecycle|DO\s+UPDATE)\b(\s+[a-z_]+)?")
def stmt(f, line):
    L = open(ROOT + f).read().split("\n")
    verbs = []
    for k in range(line - 1, max(-1, line - 80), -1):
        s = L[k]
        for m in VERB.finditer(s):
            verbs.append((k + 1, m.group(0).strip()))
        if re.search(r'sql!\(|format!\(|const [A-Z_]+: &str = |r#"|concat!\(', s):
            break
    verbs = list(reversed(verbs))
    # forward to the end of the statement string literal
    for k in range(line - 1, min(len(L), line + 80)):
        s = L[k]
        if k > line - 1:
            for m in VERB.finditer(s):
                verbs.append((k + 1, m.group(0).strip()))
        if k == line - 1:
            if re.search(r'\S"\s*(\)|,|;)?\s*$', s) and s.count('"') >= 2 - (0 if s.strip().startswith('"') else 1):
                break
        elif re.search(r'"\s*(\)|,|;)?\s*$', s):
            break
    return verbs
TABLE = re.compile(r"\b(?:FROM|UPDATE|INSERT\s+INTO|JOIN)\s+([a-z_]+)")
def nearest_table(f, line):
    L = open(ROOT + f).read().split("\n")
    for k in range(line - 1, max(-1, line - 60), -1):
        ms = TABLE.findall(L[k])
        if ms:
            return ms[-1]
    return "?"

SUB = "crates/libs/api/src/controllers/substrate/"

def collect():
    """Every line of a production file matching any PATTERNS entry, with its
    status: code, comment (archcheck: starts with //), cfgtest (inside an
    inline test-only item) or testmodfile (file declared by `#[cfg(test)] mod`)."""
    rx = {k: re.compile(v) for k, v in PATTERNS.items()}
    files = prod_files(".")
    test_mods = cfg_test_module_files(".", files)
    rows = []
    for f in files:
        text = open(f, encoding="utf-8").read().lstrip("\ufeff")
        ex = cfg_test_lines(text)
        for i, l in enumerate(text.split("\n"), 1):
            ps = [k for k, r in rx.items() if r.search(l)]
            if not ps:
                continue
            s = l.strip()
            st = ("testmodfile" if f in test_mods else "cfgtest" if i in ex
                  else "comment" if s.startswith("//") else "code")
            rows.append(dict(file=f, line=i, ins="yes" if f.startswith(SUB) else "no",
                             pats=",".join(ps), status=st, text=s))
    return rows

# ---------------------------------------------------------------- SQL helpers
PRED = re.compile(r"\b(?:(\w+)\.)?managed_by\s*(=|<>|!=|IN)\s*(\('space',\s*'k8s'\)|'k8s'|'space'|\$\d+|'autoscaler')")
def sql_table(alias, f, line):
    if alias in ("s", "sandboxes"):
        return "sandboxes"
    if alias in ("n", "nodes", "candidate"):
        return "nodes"
    if alias in ("c", "kubernetes_pool_configs"):
        return "kubernetes_pool_configs"
    return nearest_table(f, line)

def writer(f, line):
    vs = [v for _, v in stmt(f, line)]
    return any(re.match(r"(UPDATE|DELETE|INSERT|DO UPDATE|SET lifecycle)", v) for v in vs)

def pred_name(op, val, table):
    node = table == "nodes"
    if val.startswith("$"):
        return "MANAGED_BY_ACTIVE"
    if val.startswith("("):
        return "ANY_MANAGED_BY"
    base = {"'k8s'": "K8S", "'space'": "SPACE"}[val]
    pre = "NODE_" if node else ""
    if op in ("<>", "!="):
        return f"{pre}NOT_MANAGED_BY_{base}"
    return f"{pre}MANAGED_BY_{base}"

# --------------------------------------------------------------- rule tables
# Each rule: (file regex, line set or None, text regex or None) -> dict
R = []
def rule(f, lines=None, text=None, **kw):
    R.append((re.compile(f), set(lines) if lines else None, re.compile(text) if text else None, kw))

A = dict(category="a", proposed_replacement="leave alone (data)")
X = dict(category="x", proposed_replacement="none: pattern hit, not a substrate construct")
F = dict(category="f", proposed_replacement="document; keep out of the ratchet (node-side)")

# --- not substrate (x) --------------------------------------------------------
rule(r"common/svc/src/auth/", **X, notes="cargo feature `k8s` / auth decision label for Kubernetes TokenReview auth, not the substrate")
rule(r"common/svc/src/lib.rs", **X, notes="cargo feature `k8s` (Kubernetes auth), not the substrate")
rule(r"common/grpc-lb/src/lib.rs", **X, notes="cargo feature `k8s` (Kubernetes endpoint discovery), not the substrate")
rule(r"credential/mgr/server/", **X, notes="auth decision metric label for Kubernetes service-account auth, not the substrate")
rule(r"common/svc/src/federated_identity.rs", **X, notes="default issuer string \"space\"")
rule(r"cluster/controller/server/src/lib.rs", text=r"DEFAULT_LEASE_NAMESPACE", **X, notes="default namespace string \"space\"")
rule(r"node_service_readiness/backend.rs", text=r'"space"', **X, notes="default namespace string \"space\"")
rule(r"common/core/src/k8s.rs", **X, notes="RuntimeClass name \"space\"")
rule(r"node/mgr/server/", **X, notes="nftables table / path component named \"space\"")
rule(r"node/runtime/core/", **X, notes="Linux object name \"space\"")
rule(r"observability/src/sink/mod.rs", **X, notes="metrics namespace \"space\"")
rule(r"apps/node/runtime/manager/cli/", **X, notes="runtime-manager artifact management column (\"space\"-managed artifact), not the substrate")
rule(r"data/plane/server/src/runtime_route.rs", **X, notes="ECW route transport \"direct\", not the Direct substrate")
rule(r"metrics/src/data_plane.rs", **X, notes="service route transport \"direct\", not the Direct substrate")
rule(r"internal/api/server/src/metadata.rs", text=r'"direct"', **X, notes="port bridge status \"direct\", not the Direct substrate")
rule(r"api/src/controllers/sandbox/mod.rs", text=r"SANDBOX_PORT_BRIDGE_DIRECT", **X, notes="port bridge status \"direct\", not the Direct substrate")
rule(r"node/api/client/src/cluster.rs", **X, notes="node-api client Kubernetes Service endpoint discovery (cargo feature `kubernetes` of this crate), not the substrate")
rule(r"common/core/src/lib.rs", **X, notes="cargo feature gating the shared pod-rendering module k8s.rs; a module boundary, not a substrate branch")
rule(r"cluster/pool/src/(models|rows|store).rs", text=r"managed_by", **X, notes="kubernetes_pool_configs.managed_by = PoolConfigOwner ('manual'|'autoscaler'), unrelated to the substrate")
rule(r"api/src/models.rs", text=r"PoolConfigOwner", **X, notes="kubernetes_pool_configs.managed_by = PoolConfigOwner, unrelated to the substrate")
rule(r"cluster/controller/server/src/warm_pool.rs", text=r"managed_by", **X, notes="pool config owner (PoolConfigOwner), unrelated to the substrate")
rule(r"metrics/src/warm_pool.rs", text=r"managed_by", category="x", proposed_replacement="none: pattern hit, not a substrate construct",
     notes="metric tag managed_by = pool config owner ('manual'|'autoscaler'), not the substrate")

# --- node-side (f) ------------------------------------------------------------
rule(r"sbx/mgr/server/", **F, notes="sbx-mgr node-local: route identity substrate_kind / SPACE_NODE_MANAGED_BY (typed NodeManagedBy) in node SQLite and gRPC")
rule(r"net/gateway/server/", **F, notes="gateway route identity substrate_kind (proto SandboxSubstrateKind <-> NodeManagedBy)")
rule(r"apps/net/gateway/", **F, notes="gateway CLI/bench route identity substrate_kind")
rule(r"node/api/sim/", **F, notes="node-api simulator: route identity substrate_kind on the simulated worker")
rule(r"node/ctl/server/", **F, notes="node-ctl cargo feature `kubernetes` (node agent), node-local")
rule(r"common/core/src/types/sandbox.rs", **F, notes="SpaceApiRouteIdentity.substrate_kind: route identity shared by sbx-mgr, gateway and api; typed NodeManagedBy; validates pod_uid iff Kubernetes")
rule(r"common/svc/src/space_api_route.rs", **F, notes="route-token claim substrate_kind compared with the node-local identity")
rule(r"internal/api/server/src/route_identity.rs", **F, notes="route identity substrate_kind assembled from node report fields")
rule(r"internal/api/server/src/sandbox_service_ports.rs", **F, notes="route identity substrate_kind passed through")

# --- type definition (a) --------------------------------------------------------
rule(r"common/core/src/types/node.rs", **A, notes="NodeManagedBy definition and its string constants (the existing typed substrate value)")
rule(r"common/core/src/types/admin.rs", **A, notes="admin node struct field (nodes.managed_by)")

# --- metrics/log labels (e) ---------------------------------------------------
rule(r"metrics/src/sandbox/api.rs", category="e", proposed_replacement="leave alone (metric label)",
     notes="metric tag backend=\"k8s\" emitted only on Kubernetes code paths")
rule(r"metrics/src/sandbox/worker.rs", category="e", proposed_replacement="leave alone (metric label)",
     notes="auto-lifecycle delete reason label value direct_unsupported")
rule(r"api/src/wake_trace.rs", category="e", proposed_replacement="leave alone (trace field)",
     notes="cfg(feature = \"kubernetes\") gates the restore_waiter_wait_ms span field timer; only the Kubernetes restore poller records it")
rule(r"internal/api/server/src/data_plane_sessions.rs", lines=[1003, 1010], category="e", proposed_replacement="leave alone (error/log text)",
     notes="managed_by={..} interpolated into an error message")
rule(r"transient_lifecycle_sweeper/store.rs", lines=[746], category="e", proposed_replacement="leave alone (error text)",
     notes="error message naming managed_by")

# --- Rust capability branches (d) -----------------------------------------------
def D(cap, repl, notes, **kw):
    return dict(category="d", capability=cap, proposed_replacement=repl, notes=notes, **kw)
rule(r"api/src/controllers/sandbox/lifecycle.rs", lines=[366, 367, 368],
     **D("claims_restoring_lifecycle", "SubstrateCapabilities field",
         "PublishedSuspendFence::for_substrate: Kubernetes claims restoring before its node call, so a finalized suspended state is fenced; Direct keeps suspended as its restore baseline (doc on complete_wake_lifecycle)"))
rule(r"api/src/controllers/sandbox/wake_completion.rs", lines=[95, 96, 97],
     **D("claims_restoring_lifecycle", "SubstrateCapabilities field",
         "shield_applies: wake-cancellation shield only for Kubernetes restore-shaped wakes; Direct restore never leaves suspended mid-flight (doc comment)"))
rule(r"api/src/controllers/sandbox/wake_completion.rs", lines=[256, 257, 258],
     **D("has_pod_routability_contract", "SubstrateAdapter method (hydrate_restored_execution with a Direct default of Routable)",
         "wake_completion_redrive: Direct is always routable; Kubernetes hydrates the execution from the live pod"))
rule(r"transient_lifecycle_sweeper/store.rs", lines=[952, 953, 954],
     **D("has_pod_routability_contract", "SubstrateAdapter method (hydrate_restored_execution with a Direct default of Routable)",
         "ensure_routable_execution: same Direct=Routable / Kubernetes=hydrate split as wake_completion_redrive"))
rule(r"api/src/fork_operation_executor.rs", lines=[61, 62, 63],
     **D("node_addressing", "SubstrateCapabilities field (node address kind: direct endpoint vs substrate node id)",
         "copy_node_endpoint_requirement: Direct needs node_direct endpoint, Kubernetes needs substrate node id"))
rule(r"cluster/controller/server/src/reconcilers.rs", lines=[391, 394],
     **D("owns_pod_lifecycle", "SubstrateCapabilities field; keep the Kubernetes reconciler set typed on KubernetesSubstrate",
         "spawn Kubernetes-only reconcilers (warm pool, orphaned sandboxes, drain, GC...) only on Kubernetes"))
rule(r"cluster/controller/server/src/lib.rs", lines=[145],
     **D("has_leader_election_lease", "SubstrateAdapter accessor returning an optional lease store",
         "leader election uses a Kubernetes Lease only on the Kubernetes substrate"))
rule(r"api/src/controllers/sandbox/queries.rs", lines=[606],
     **D("uses_pool_configured_shapes", "SubstrateCapabilities field",
         "cold-create shape: only Kubernetes Firecracker creates read pool-configured resources (checks the substrate's managed_by string)"))
rule(r"api/src/controllers/sandbox/spaced.rs", lines=[75],
     **D("wakes_dormant_on_connect", "SubstrateCapabilities field (or row predicate on the typed NodeManagedBy)",
         "fallback wake_execution for a dormant sandbox only when the row's managed_by is k8s"))
rule(r"sandbox/store/src/lib.rs", lines=[485],
     **D("proves_execution_absence", "row predicate on typed NodeManagedBy + capability",
         "AutoLifecycleExecution::proves_no_execution: only Kubernetes publishes activation ownership before launch"))
rule(r"api/src/routes/admin/node_maintenance.rs", lines=[431, 432, 998, 1192, 1329, 1581, 1712],
     **D("registers_nodes_via_admin_api", "SubstrateCapabilities field",
         "require_direct_substrate: admin node register/update/delete routes are Direct-only"))
rule(r"api/src/routes/session_auth.rs", lines=[31, 32, 33],
     **D("(none: response label)", "map from typed NodeManagedBy",
         "ConsoleSubstrate::from_managed_by: string match turning managed_by into the console's substrate field; anything not \"k8s\" is Space"))
rule(r"api/src/routes/session_auth.rs", lines=[143], category="a", proposed_replacement="leave alone (API response field)",
     notes="/me response substrate field; fed by SubstrateAdapter::managed_by()")
rule(r"cluster/trigger-queue/src/lib.rs", lines=[265],
     **D("has_node_inventory", "row predicate on typed NodeManagedBy (already typed)",
         "trigger targets: only Kubernetes nodes are dead-lettered when absent from live_node_names()"))
rule(r"orphaned_restore_pod_gc/store.rs", lines=[22, 23],
     category="b", capability="MANAGED_BY_K8S", proposed_replacement="push into the SQL WHERE as the named predicate, or match typed NodeManagedBy",
     notes="sandboxes row: Rust-side filter; non-k8s rows are Unrecognized and skipped")
rule(r"orphaned_restore_pod_gc/store.rs", lines=[65, 77, 81], **A, notes="sandboxes.managed_by projected for the Rust-side filter above")
rule(r"api/src/routes/admin/node_maintenance.rs", lines=[1210],
     **D("registers_nodes_via_admin_api", "row predicate on typed NodeManagedBy (already typed)",
         "node row: admin update only touches live Direct nodes"))
rule(r"api/src/controllers/sandbox/lifecycle.rs", lines=[1216, 1217, 1219, 1220],
     category="a", proposed_replacement="leave alone (data); note default",
     notes="observed node managed_by parsed into NodeManagedBy with an unwrap_or(Space) default")

# --- concrete substrate types outside the module (b) ----------------------------
rule(r".", text=r"\bKubernetesSubstrate\b", category="b", capability="KubernetesSubstrate (typed dependency)",
     proposed_replacement="keep typed on KubernetesSubstrate; wiring gated by owns_pod_lifecycle",
     notes="Kubernetes-only reconciler/backend holding the concrete substrate")
rule(r".", text=r"\bDirectSubstrate\b", category="b", capability="DirectSubstrate (typed dependency)",
     proposed_replacement="keep", notes="concrete Direct substrate outside the module")
rule(r"node_drain/store.rs", lines=[1592, 1608], category="b", capability="KubernetesSubstrate (typed dependency)",
     proposed_replacement="keep", notes="SubstrateSinkBackend: Kubernetes or suspend-only backend for the drain sink")
rule(r"reconcilers.rs", lines=[199], category="b", capability="TriggerSubstrate",
     proposed_replacement="keep", notes="TriggerSubstrate wraps the Substrate for trigger fan-out (uses live_node_names())")
rule(r"reconcilers.rs", lines=[166], **A, notes="trigger target managed_by (typed NodeManagedBy)")

# --- cfg(feature = "kubernetes") gates in shared crates -----------------------------
rule(r"cluster/pool/src/lib.rs", category="b", capability="cfg(feature = \"kubernetes\")",
     proposed_replacement="keep: compile-time boundary of the Kubernetes-only warm-pool crate",
     notes="whole warm-pool store compiled only with the kubernetes feature")
rule(r"api/src/lib.rs", category="b", capability="cfg(feature = \"kubernetes\")",
     proposed_replacement="move under the substrate module or keep as a compile-time boundary",
     notes="node_group_placement module compiled only with kubernetes")
rule(r"cluster/controller/server/src/.*(drained_node_terminator|node_drain|node_group_labeler|orphaned_nodes|reconcile_queue_gc)/", text=r'feature = "kubernetes"',
     category="b", capability="cfg(feature = \"kubernetes\")",
     proposed_replacement="keep: compile-time half of a Kubernetes-only reconciler/backend",
     notes="cfg gate on a Kubernetes-only backend or import in a cluster-controller reconciler")
rule(r"transient_lifecycle_sweeper/store.rs", lines=[8], category="b", capability="cfg(feature = \"kubernetes\")",
     proposed_replacement="disappears with the hydrate_restored_execution capability", notes="import for the Kubernetes arm at 952-954")

# --- Kubernetes-only finalization SQL helpers (c) -----------------------------------
rule(r".", text=r"\b(suspended_k8s_finalization_sql|quarantined_k8s_finalization_sql|k8s_finalization_sql|SuspendedK8sFinalizationSql)\b",
     category="c", capability="MANAGED_BY_K8S (callers)",
     proposed_replacement="keep the helper; callers gate with the named predicate",
     notes="sandboxes + sandbox_k8s_executions writer fragment that is Kubernetes-only by name; callers pair it with managed_by = 'k8s'")
rule(r".", text=r"\b(routable_execution_or_non_k8s)\b", category="c", capability="NOT_MANAGED_BY_K8S",
     proposed_replacement="named predicate inside the shared routability fragment",
     notes="started-convergence gate: routable k8s execution OR not a k8s row")

# --- remaining managed_by data -----------------------------------------------------
rule(r"api/src/controllers/sandbox/mod.rs", lines=[1121, 1122], **A, notes="SandboxController::managed_by() pass-through of SubstrateAdapter::managed_by()")
rule(r"internal/api/server/src/lib.rs", lines=[1327], **A, notes="INSERT nodes value 'k8s' (Kubernetes node registration)")
rule(r"api/src/routes/admin/node_maintenance.rs", lines=[1124], **A, notes="INSERT nodes value 'space' (Direct node registration)")
rule(r"db-test-fixtures/src/lib.rs", lines=[158], **A, notes="test fixture crate: INSERT nodes value 'k8s'")

rule(r"api/src/routes/public/sandboxes.rs", lines=[900], category="c", capability="MANAGED_BY_K8S",
     proposed_replacement="named predicate; candidate for a substrate-owned 'current execution identity' fragment",
     notes="sandboxes projection gating: failed-observation match against the active sandbox_k8s_executions row")
rule(r"api/src/routes/public/sandboxes.rs", lines=[918], category="c", capability="MANAGED_BY_SPACE",
     proposed_replacement="named predicate; candidate for a substrate-owned 'current execution identity' fragment",
     notes="sandboxes projection gating: failed-observation match against the Direct route identity")
rule(r"(failed_restorable_repair|started_sandbox_repair)/store.rs", lines=[45, 47], category="c", capability="MANAGED_BY_K8S",
     proposed_replacement="named predicate (phase 2)",
     notes="sandboxes row_state_guards shared by the scan and the lifecycle-writing apply; Kubernetes-only repair")
rule(r"drained_node_terminator/store.rs", lines=[303], category="c", capability="NODE_MANAGED_BY_K8S",
     proposed_replacement="named predicate (phase 2)",
     notes="nodes.managed_by = 'k8s' inside an UPDATE sandboxes SET lifecycle = 'failed' (suspended rows on a terminated drained Kubernetes node)")

rule(r"common/svc/src/sql/sandbox_routability.rs", lines=[55], category="c", capability="NOT_MANAGED_BY_K8S",
     proposed_replacement="named predicate inside the shared routability fragment",
     notes="sandboxes.managed_by <> 'k8s' (alias supplied by caller): rows with no routability contract may converge to started")

rule(r"transient_lifecycle_sweeper/store.rs", text=r"(let managed_by = self\.substrate\.managed_by\(\)|\.bind\(managed_by\))",
     category="b", capability="MANAGED_BY_ACTIVE",
     proposed_replacement="bind a typed active-substrate value (phase 2); disappears if the scope becomes a predicate helper",
     notes="Rust half of the sweeper's own-substrate scope: binds SubstrateAdapter::managed_by() into a statement that filters sandboxes.managed_by = $n")

rule(r"api/src/routes/admin/node_maintenance.rs", lines=[1050], **A,
     notes="UPDATE nodes SET managed_by = 'space' value; redundant, the candidate CTE already requires n.managed_by = 'space'")

def classify(r):
    f, l, t = r["file"], r["line"], r["text"]
    for fr, ls, tr, kw in R:
        if not fr.search(f):
            continue
        if ls is not None and l not in ls:
            continue
        if tr is not None and not tr.search(t):
            continue
        return dict(kw)
    m = PRED.search(t)
    if m:
        alias, op, val = m.groups()
        table = sql_table(alias, f, l)
        w = writer(f, l)
        name = pred_name(op, val, table)
        if name == "ANY_MANAGED_BY":
            return dict(category="b", capability=name, proposed_replacement="delete: always true under the CHECK constraint",
                        notes=f"{table}.managed_by IN ('space','k8s') is every allowed value (NOT NULL + CHECK); dead filter")
        cat = "c" if w else "b"
        what = "lifecycle/row writer guard" if w else "query scope"
        return dict(category=cat, capability=name, proposed_replacement="named predicate (phase 2)",
                    notes=f"{table}.managed_by {op} {val}: {what}")
    if re.search(r"managed_by", t):
        return dict(category="a", proposed_replacement="leave alone (data)", notes="")
    if re.search(r"substrate_kind", t):
        return dict(category="a", proposed_replacement="leave alone (data)",
                    notes="route identity substrate_kind carried as data")
    return None

def write_csv(path):
    rows = [r for r in collect() if r["status"] == "code"
            and (r["ins"] == "no" or "managed_by" in r["pats"].split(","))]
    out = []
    unclassified = []
    for r in rows:
        c = classify(r)
        if c is None:
            unclassified.append(r)
            continue
        construct = r["text"] if len(r["text"]) <= 80 else r["text"][:77] + "..."
        out.append([r["file"], r["line"], r["ins"], c["category"], construct,
                    c.get("proposed_replacement", ""), c.get("capability", ""), c.get("notes", "")])
    if unclassified:
        for r in unclassified:
            print("UNCLASSIFIED", r["file"], r["line"], r["text"][:100])
    w = csv.writer(open(path, "w", newline=""), lineterminator="\n")
    w.writerow(["file", "line", "in_substrate_module", "category", "construct", "proposed_replacement", "capability", "notes"])
    w.writerows(out)
    print(len(out), "rows")

# ---- phase 1 ratchet proposal
from collections import Counter, defaultdict

RATCHET_PATTERNS = {
    # SQL: a managed_by comparison against a substrate value or a bind.
    "sql_managed_by": r"\bmanaged_by\s*(?:=|<>|!=)\s*(?:'(?:k8s|space)'|\$\d+)(?!\s*,)|\bmanaged_by\s+IN\s*\(\s*'(?:k8s|space)'",
    # Rust: comparing or matching a managed_by string against a substrate value.
    "rust_managed_by_str": r"(?:==|!=)\s*\"(?:k8s|space)\"|^\s*\"(?:k8s|space)\"\s*=>",
    # Rust: matching on the api Substrate enum outside its module.
    "substrate_variant": r"\bSubstrate::(?:Direct|Kubernetes)\b",
    # Rust: comparing a typed NodeManagedBy value.
    "node_managed_by_cmp": r"(?:==|!=)\s*(?:Some\()?(?:[\w:]*::)?NodeManagedBy::(?:Space|Kubernetes)\b",
}
ALLOW = [
    r"^crates/libs/api/src/controllers/substrate/",   # the substrate module itself
    r"^crates/libs/sbx/mgr/",                          # node-side (f)
    r"^crates/libs/net/gateway/", r"^crates/apps/net/gateway/",
    r"^crates/libs/node/",
]
rrx = {k: re.compile(v) for k, v in RATCHET_PATTERNS.items()}
allow = [re.compile(a) for a in ALLOW]

def scan(exclude_test_only):
    files = prod_files(".")
    tmods = cfg_test_module_files(".", files) if exclude_test_only else set()
    hits = []
    for f in files:
        if f in tmods or any(a.search(f) for a in allow):
            continue
        text = open(f, encoding="utf-8").read().lstrip("﻿")
        ex = cfg_test_lines(text) if exclude_test_only else set()
        for i, l in enumerate(text.split("\n"), 1):
            s = l.strip()
            if not s or s.startswith("//") or i in ex:
                continue
            ps = [k for k, r in rrx.items() if r.search(l)]
            if ps:
                hits.append((f, i, ps, s))
    return hits

def ratchet(csv_path, verbose=False):
    a = scan(False); b = scan(True)
    print(f"archcheck file-level scope: {len(a)} lines; with test-only items removed: {len(b)} lines")
    sa = {(f, i) for f, i, _, _ in a}; sb = {(f, i) for f, i, _, _ in b}
    for f, i, _, s in a:
        if (f, i) not in sb:
            print("  only in file-level scope:", f, i, s[:70])
    cat = {(r["file"], int(r["line"])): r["category"] for r in csv.DictReader(open(csv_path))}
    pc = Counter(); byp = defaultdict(Counter)
    for f, i, ps, s in b:
        c = cat.get((f, i), "NOT-IN-CSV")
        pc[c] += 1
        for p in ps: byp[p][c] += 1
    print("precision by category:", dict(pc))
    for p in RATCHET_PATTERNS: print(f"  {p}: {dict(byp[p])}")
    print("\nper-file baseline (test-only items removed):")
    per = Counter(f for f, _, _, _ in b)
    perp = defaultdict(Counter)
    for f, _, ps, _ in b:
        for p in ps: perp[f][p] += 1
    keys = list(RATCHET_PATTERNS)
    print("| ratchet file | " + " | ".join(f"`{k}`" for k in keys) + " | lines |")
    print("|---|" + "---:|" * (len(keys) + 1))
    for f, n in sorted(per.items()):
        print(f"| `{f}` | " + " | ".join(str(perp[f][k] or "") for k in keys) + f" | {n} |")
    print(f"| **total ({len(per)} files)** | " + " | ".join(str(sum(perp[f][k] for f in perp)) for k in keys) + f" | {sum(per.values())} |")
    hit = {(f, i) for f, i, _, _ in b}
    miss = Counter()
    for r in csv.DictReader(open(csv_path)):
        if r["in_substrate_module"] == "no" and r["category"] in "bcd" and (r["file"], int(r["line"])) not in hit:
            miss[(r["category"], r["capability"] or r["notes"][:40])] += 1
    print("\nb/c/d rows outside the module NOT caught, by category and capability:")
    for (c, k), n in sorted(miss.items()): print(f"  {c} {n:3}  {k}")
    print(f"  total {sum(miss.values())}")
    if verbose:
        for f, i, ps, s in b:
            print(cat.get((f, i), "NOT-IN-CSV"), f"{f}:{i}", ",".join(ps), s[:70])

# ---- report tables
def counts(CSV):
    rows = list(csv.DictReader(open(CSV, newline="")))
    cats = "abcdefx"
    members = tomllib.load(open("Cargo.toml", "rb"))["workspace"]["members"]
    pkg = {m: tomllib.load(open(f"{m}/Cargo.toml", "rb"))["package"]["name"] for m in members}
    def crate(f):
        best = max((m for m in members if f.startswith(m + "/")), key=len)
        return pkg[best]
    def table(counter_by_key, keys, title):
        print(f"| {title} | " + " | ".join(keys) + " | total |")
        print("|---|" + "---:|" * (len(keys) + 1))
        for k, c in counter_by_key:
            print(f"| {k} | " + " | ".join(str(c.get(x, 0)) for x in keys) + f" | {sum(c.values())} |")
    print(f"rows: {len(rows)}")
    ov = defaultdict(Counter)
    for r in rows:
        ov["inside substrate module" if r["in_substrate_module"] == "yes" else "outside substrate module"][r["category"]] += 1
        ov["all"][r["category"]] += 1
    table([(k, ov[k]) for k in ("outside substrate module", "inside substrate module", "all")], list(cats), "scope")
    print()
    mb = defaultdict(Counter)
    for r in rows:
        if "managed_by" in open(r["file"]).read().split("\n")[int(r["line"]) - 1]:
            mb["inside" if r["in_substrate_module"] == "yes" else "outside"][r["category"]] += 1
    table([(k, mb[k]) for k in ("outside", "inside")], list(cats), "`managed_by` rows")
    print()
    pc = defaultdict(Counter)
    for r in rows:
        pc[crate(r["file"])][r["category"]] += 1
    table(sorted(pc.items(), key=lambda kv: -sum(kv[1].values())), list(cats), "crate")
    print()
    pf = defaultdict(Counter)
    for r in rows:
        if r["category"] in "bcd":
            pf[r["file"]][r["category"]] += 1
    top = sorted(pf.items(), key=lambda kv: (-sum(kv[1].values()), kv[0]))[:12]
    table(top, list("bcd"), "file (b+c+d)")
    print()
    cap = defaultdict(Counter)
    for r in rows:
        if r["category"] in "bcd" and r["capability"]:
            cap[r["capability"]][r["category"]] += 1
    table(sorted(cap.items(), key=lambda kv: -sum(kv[1].values())), list("bcd"), "capability / predicate")

def sqlform(CSV):
    rows = list(csv.DictReader(open(CSV)))
    form = Counter(); detail = []
    for r in rows:
        if r["category"] not in "bc" or not re.search(r"managed_by\s*(=|<>|!=|IN)\s*['$(]", r["construct"]):
            continue
        L = open(r["file"]).read().split("\n")
        kind = "?"
        for k in range(int(r["line"]) - 1, max(-1, int(r["line"]) - 120), -1):
            s = L[k]
            if re.search(r"sql!\(\s*$|sql!\(\s*\"", s): kind = "sql!(literal)"; break
            if re.search(r"format!\(", s): kind = "format!(runtime String)"; break
            if re.search(r"const [A-Z_]+: &str = \"", s): kind = "const &str (not wrapped)"; break
        form[kind] += 1; detail.append((kind, r["file"], r["line"]))
    for k, v in form.most_common(): print(f"{v:4} {k}")
    for d in detail:
        if d[0] != "sql!(literal)": print(*d)

def reconcile():
    """managed_by line counts at each scope step, inside/outside the module."""
    rx = re.compile(PATTERNS["managed_by"])
    files = prod_files(".")
    tmods = cfg_test_module_files(".", files)
    step = Counter()
    for f in files:
        text = open(f, encoding="utf-8").read().lstrip("﻿")
        ex = cfg_test_lines(text)
        where = "inside" if f.startswith(SUB) else "outside"
        for i, l in enumerate(text.split("\n"), 1):
            if not rx.search(l):
                continue
            step[("1 archcheck production files, physical lines", where)] += 1
            if f in tmods:
                step[("removed: file declared by #[cfg(test)] mod", where)] += 1
                continue
            if i in ex:
                step[("removed: inside inline test-only item", where)] += 1
                continue
            if l.strip().startswith("//"):
                step[("removed: // comment line (not an archcheck code line)", where)] += 1
                continue
            step[("2 in scope (code lines)", where)] += 1
    for k in sorted({k for k, _ in step}):
        o, n = step[(k, "outside")], step[(k, "inside")]
        print(f"{k:58} outside={o:4} inside={n:4} total={o + n:4}")
    print("all inventory patterns, by status:", dict(Counter(r["status"] for r in collect())))

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "csv": write_csv(sys.argv[2])
    elif cmd == "reconcile": reconcile()
    elif cmd == "counts": counts(sys.argv[2])
    elif cmd == "sqlform": sqlform(sys.argv[2])
    elif cmd == "ratchet": ratchet(sys.argv[2], "-v" in sys.argv)
    else: sys.exit(f"unknown command {cmd}")
```

</details>

### Reconciliation with the reference counts

The reference commands are:

```sh
git grep -n managed_by -- '*.rs' ':!*test*'                                                    # 335
git grep -n managed_by -- '*.rs' ':!*test*' ':!crates/libs/api/src/controllers/substrate/'     # 299
```

`python3 inventory.py reconcile` gives:

```text
1 archcheck production files, physical lines               outside= 307 inside=  36 total= 343
2 in scope (code lines)                                    outside= 263 inside=  36 total= 299
removed: // comment line (not an archcheck code line)      outside=  37 inside=   0 total=  37
removed: file declared by #[cfg(test)] mod                 outside=   7 inside=   0 total=   7
all inventory patterns, by status: {'code': 826, 'comment': 64, 'testmodfile': 13, 'cfgtest': 4}
```

How the numbers relate:

| step | outside module | inside | total |
|---|---:|---:|---:|
| reference `git grep … ':!*test*'` | 299 | 36 | 335 |
| + lines in archcheck production files whose path contains `test` | +8 | 0 | 343 |
| − files declared by a test-only `mod x;` | −7 | 0 | 336 |
| − inline `#[cfg(test)]` items | 0 | 0 | 336 |
| − `//` comment lines | −37 | 0 | **299** |
| **classified `managed_by` rows** | **263** | **36** | **299** |

- **The +8.** These lines are in archcheck production files that the
  reference pathspec drops because their path contains `test`:
  - `crates/libs/cluster/controller/server/src/lifecycle_writer_test_support.rs`:
    7 lines. Production by filename, but declared `#[cfg(test)] mod`.
  - `crates/libs/db-test-fixtures/src/lib.rs`: 1 line. A real library crate.

  Every reference line is in archcheck's set: `comm` of the two `file:line`
  lists finds nothing only in the reference.
- **The final total equals the reference's "outside" figure.** Both are 299,
  but that is a coincidence. 299 is in-scope code lines across the whole tree,
  not lines outside the module.

**Is inline `#[cfg(test)]` exclusion needed for stable counts?** No. Across all
inventory patterns, inline test-only items remove 4 lines. All four are
`mod tests;` declarations: three `#[cfg(all(test, feature = "kubernetes"))]`
and one `#[cfg(all(test, feature = "k8s"))]`. They contain no `managed_by` lines and nothing the phase 1 ratchet
patterns match. Test-only *module files* are what differ, and only by
filename:

| scope rule | CSV-rule code lines | ratchet-pattern lines (§7) |
|---|---:|---:|
| archcheck file-level definition only | 712 | 83 |
| + test-only module files and inline items removed (used here) | 697 | 82 |

For the ratchet, the whole difference is one line in
`crates/libs/api/src/routes/public/sandboxes/admission_test_fixture.rs:166`.
That file is a `#[cfg(test)] mod` with a non-`_tests.rs` name. archcheck's
file-level definition is good enough for phase 1 if that file is either:

- listed in the baseline with a count of 1, or
- left out of the baseline, accepting one stale-looking line until it is
  renamed.

No brace-aware `#[cfg(test)]` parsing is required.

**The plan's earlier count of 33 checks.** The method behind 33 was not
recorded, so it cannot be reproduced exactly. Here, category `d` outside the
module has 33 rows, but that match is partly a coincidence of line counting.
Those 33 rows are **14 distinct decision sites**:

- 7 `Substrate::{Direct,Kubernetes}` matches (13 lines);
- 7 string or typed comparisons: `queries.rs:606`, `spaced.rs:75`,
  `sandbox/store/src/lib.rs:485`, `require_direct_substrate`
  (`node_maintenance.rs:432`, plus 5 call-site rows), `node_maintenance.rs:1210`,
  the `ConsoleSubstrate` mapper and the trigger-queue node check.

The `cfg(feature = "kubernetes")` line on each match arm is counted with the
arm. By construct type, the explicit checks outside the module are:

- 13 `Substrate::` variant lines;
- 6 Rust `== / != "k8s"|"space"` comparisons or match arms;
- 2 typed `NodeManagedBy` comparisons in control-plane code;
- 61 SQL `managed_by` predicate lines in `b`/`c` outside the module (28 `b`,
  33 `c`), all of which the ratchet pattern in §7 matches.

The 33 most likely counted Rust checks only. SQL scoping adds 61 more lines,
nearly twice as many.

## 2. Category counts

`python3 inventory.py counts reports/direct-substrate-inventory/inventory.csv`:

| scope | a | b | c | d | e | f | x | total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| outside substrate module | 132 | 142 | 63 | 33 | 18 | 160 | 113 | 661 |
| inside substrate module | 18 | 14 | 4 | 0 | 0 | 0 | 0 | 36 |
| all | 150 | 156 | 67 | 33 | 18 | 160 | 113 | 697 |

| `managed_by` rows | a | b | c | d | e | f | x | total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| outside | 115 | 60 | 33 | 8 | 3 | 10 | 34 | 263 |
| inside | 18 | 14 | 4 | 0 | 0 | 0 | 0 | 36 |

| crate | a | b | c | d | e | f | x | total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pplx-space-cluster-controller-server | 11 | 121 | 35 | 6 | 1 | 0 | 10 | 184 |
| pplx-space-api | 97 | 17 | 11 | 25 | 9 | 0 | 3 | 162 |
| pplx-space-sbx-mgr | 0 | 0 | 0 | 0 | 0 | 91 | 0 | 91 |
| pplx-space-common-svc | 0 | 0 | 2 | 0 | 0 | 5 | 53 | 60 |
| pplx-space-internal-api-server | 23 | 5 | 8 | 0 | 2 | 6 | 1 | 45 |
| pplx-space-cluster-pool | 0 | 12 | 0 | 0 | 0 | 0 | 18 | 30 |
| pplx-space-common-core | 8 | 0 | 0 | 0 | 0 | 20 | 2 | 30 |
| pplx-space-net-gateway | 0 | 0 | 0 | 0 | 0 | 20 | 0 | 20 |
| pplx-space-metrics | 0 | 0 | 0 | 0 | 6 | 0 | 5 | 11 |
| pplx-space-sandbox-portability | 0 | 0 | 11 | 0 | 0 | 0 | 0 | 11 |
| pplx-space-node-api-sim | 0 | 0 | 0 | 0 | 0 | 10 | 0 | 10 |
| pplx-space-node-api-client | 0 | 0 | 0 | 0 | 0 | 0 | 6 | 6 |
| pplx-space-node-ctl-server | 0 | 0 | 0 | 0 | 0 | 5 | 0 | 5 |
| pplx-space-sandbox-store | 4 | 0 | 0 | 1 | 0 | 0 | 0 | 5 |
| pplx-space-cluster-trigger-queue | 2 | 1 | 0 | 1 | 0 | 0 | 0 | 4 |
| pplx-space-credmgr | 0 | 0 | 0 | 0 | 0 | 0 | 4 | 4 |
| pplx-space-runtime-manager-cli | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 3 |
| pplx-space-node-mgr | 0 | 0 | 0 | 0 | 0 | 0 | 3 | 3 |
| pplx-space-net-gateway-svc | 0 | 0 | 0 | 0 | 0 | 2 | 0 | 2 |
| pplx-space-cluster-reconciler | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 2 |
| pplx-space-common-grpc-lb | 0 | 0 | 0 | 0 | 0 | 0 | 2 | 2 |
| pplx-space-db-test-fixtures | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 2 |
| pplx-space-admin-cli | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 1 |
| pplx-space-netgtw-bench | 0 | 0 | 0 | 0 | 0 | 1 | 0 | 1 |
| pplx-space-data-plane-server | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 |
| pplx-space-node-runtime-core | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 |
| pplx-space-observability | 0 | 0 | 0 | 0 | 0 | 0 | 1 | 1 |

| file (b+c+d) | b | c | d | total |
|---|---:|---:|---:|---:|
| crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs | 36 | 16 | 3 | 55 |
| crates/libs/api/src/controllers/substrate/direct.rs | 12 | 0 | 0 | 12 |
| crates/libs/cluster/controller/server/src/orphaned_sandboxes.rs | 8 | 4 | 0 | 12 |
| crates/libs/cluster/pool/src/lib.rs | 12 | 0 | 0 | 12 |
| crates/libs/api/src/routes/admin/node_maintenance.rs | 0 | 3 | 8 | 11 |
| crates/libs/cluster/controller/server/src/node_drain/store.rs | 9 | 2 | 0 | 11 |
| crates/libs/sandbox/portability/src/lib.rs | 0 | 11 | 0 | 11 |
| crates/libs/api/src/controllers/sandbox/wake_completion.rs | 0 | 2 | 6 | 8 |
| crates/libs/cluster/controller/server/src/drained_node_terminator/store.rs | 6 | 2 | 0 | 8 |
| crates/libs/cluster/controller/server/src/reconcile_queue_gc/detector.rs | 7 | 0 | 0 | 7 |
| crates/libs/cluster/controller/server/src/node_drain/mod.rs | 6 | 0 | 0 | 6 |
| crates/libs/cluster/controller/server/src/started_sandbox_repair/store.rs | 1 | 5 | 0 | 6 |

| capability / predicate | b | c | d | total |
|---|---:|---:|---:|---:|
| MANAGED_BY_ACTIVE | 34 | 11 | 0 | 45 |
| KubernetesSubstrate (typed dependency) | 44 | 0 | 0 | 44 |
| cfg(feature = "kubernetes") | 37 | 0 | 0 | 37 |
| NODE_MANAGED_BY_K8S | 12 | 11 | 0 | 23 |
| MANAGED_BY_K8S (callers) | 0 | 23 | 0 | 23 |
| NODE_MANAGED_BY_SPACE | 14 | 4 | 0 | 18 |
| MANAGED_BY_K8S | 9 | 7 | 0 | 16 |
| NOT_MANAGED_BY_K8S | 0 | 8 | 0 | 8 |
| registers_nodes_via_admin_api | 0 | 0 | 8 | 8 |
| claims_restoring_lifecycle | 0 | 0 | 6 | 6 |
| has_pod_routability_contract | 0 | 0 | 6 | 6 |
| node_addressing | 0 | 0 | 3 | 3 |
| NODE_NOT_MANAGED_BY_SPACE | 3 | 0 | 0 | 3 |
| MANAGED_BY_SPACE | 0 | 3 | 0 | 3 |
| (none: response label) | 0 | 0 | 3 | 3 |
| owns_pod_lifecycle | 0 | 0 | 2 | 2 |
| ANY_MANAGED_BY | 2 | 0 | 0 | 2 |
| uses_pool_configured_shapes | 0 | 0 | 1 | 1 |
| wakes_dormant_on_connect | 0 | 0 | 1 | 1 |
| has_leader_election_lease | 0 | 0 | 1 | 1 |
| TriggerSubstrate | 1 | 0 | 0 | 1 |
| has_node_inventory | 0 | 0 | 1 | 1 |
| proves_execution_absence | 0 | 0 | 1 | 1 |

What the tables show:

- **Most of the leak is SQL scoping in the cluster controller.**
  `transient_lifecycle_sweeper/store.rs` alone has 55 `b`/`c`/`d` rows.
- **Rust capability branches are few.** There are 33 rows at 14 sites, mostly
  in `pplx-space-api`.
- **`f` is large but node-side.** 91 of its 160 rows are sbx-mgr; gateway,
  node-api sim, node-ctl and the shared route identity type make up the rest.
- **`x` is large because of name collisions.** It is mostly the `k8s` cargo
  feature of the auth crates (53 rows in `common-svc`) and `PoolConfigOwner`'s
  `managed_by` (32 rows; 2 more `managed_by` rows are the runtime-manager
  CLI's artifact column).
- **The `cfg(feature = "kubernetes")` (37) and `KubernetesSubstrate` (44)
  rows** are the compile-time and typed halves of Kubernetes-only reconcilers.
  They live in `cluster-controller` and `cluster-pool`. They are `b`, but
  structural rather than predicates.

## 3. Proposed capability names

These are the distinct substrate questions the code asks, each with the sites
that ask it. "Row" means the code tests a row's `managed_by`, not the active
substrate.

| capability | asks | sites (file:line) | shape |
|---|---|---|---|
| `owns_pod_lifecycle` | Does the substrate own a pod lifecycle and therefore run the Kubernetes reconciler set (warm pool, orphaned sandboxes/nodes/restore pods, drain, drained-node terminator, reconcile-queue GC, stuck ContainerCreating GC, suspended-execution reaper, started/failed repair, node-group mirror/labeler, kube node sync, service readiness)? | `cluster/controller/server/src/reconcilers.rs:391,394`; the 44 typed `KubernetesSubstrate` rows are the reconcilers wired there | flag; reconcilers stay typed on `KubernetesSubstrate` |
| `claims_restoring_lifecycle` | Does a restore claim `restoring` before its node call, so a finalized `suspended` must be fenced and a cancelled wake needs the shield? | `api/src/controllers/sandbox/lifecycle.rs:366-368` (`PublishedSuspendFence`); `api/src/controllers/sandbox/wake_completion.rs:95-97` (`shield_applies`) | flag |
| `has_pod_routability_contract` | Does `started` promise a routable execution row that may need hydrating from a live pod? | `wake_completion.rs:256-258` (redrive); `cluster/controller/server/src/transient_lifecycle_sweeper/store.rs:952-954` (`ensure_routable_execution`); SQL twin `NOT_MANAGED_BY_K8S` in `common/svc/src/sql/sandbox_routability.rs:51-55` and its callers `published_state.rs:2834`, `transient_lifecycle_sweeper/store.rs:440,525`, `wake_completion.rs:231` | method `ensure_routable_execution(id, budget) -> Routability`, defaulting to `Routable`; flag for the SQL twin |
| `node_addressing` | How is a node addressed for node-api calls: a `node_direct` endpoint, or a substrate node id? | `api/src/fork_operation_executor.rs:61-63` | enum `NodeAddressing { DirectEndpoint, SubstrateNodeId }` |
| `registers_nodes_via_admin_api` | Are nodes registered, updated and deleted through the admin API, rather than discovered? | `api/src/routes/admin/node_maintenance.rs:431-432` (definition), call sites `:998, :1192, :1329, :1581, :1712`; row check `:1210` | flag |
| `uses_pool_configured_shapes` | Do cold creates read warm-pool-configured resources? | `api/src/controllers/sandbox/queries.rs:606` | flag; sibling of the existing `supports_node_pool_placement()` |
| `has_leader_election_lease` | Is reconciler leadership held through a Kubernetes Lease? | `cluster/controller/server/src/lib.rs:145` | method returning an optional lease store (it needs the kube client) |
| `has_node_inventory` | Is node membership observed from a live inventory, where absence is meaningful? | already `Substrate::live_node_names() -> Option<_>`; row check `cluster/trigger-queue/src/lib.rs:265` | existing method; the row check stays typed |
| `wakes_dormant_on_connect` (row) | Should a failed connect fall back to `wake_execution` for this row's substrate? | `api/src/controllers/sandbox/spaced.rs:75` | method on `NodeManagedBy`, or a flag if mixed rows are ruled out |
| `proves_execution_absence` (row) | Does the row's substrate publish activation ownership before launch, so its absence is proof? | `sandbox/store/src/lib.rs:485` | method on `NodeManagedBy` (`sandbox-store` cannot depend on `api`) |
| (label, not a capability) | console `/me` substrate field | `api/src/routes/session_auth.rs:31-33` | `From<NodeManagedBy>` |

These already ask capability-shaped questions and need no change:

- `supports_node_pool_placement()`;
- `live_node_names() -> Option<_>`;
- `reserve_rollback_route_identity` / `persist_rollback_route_identity`
  (Direct does work, Kubernetes no-ops);
- `retire_fence_mismatch` ("a substrate with no pod object to fence against
  always answers `None`");
- `observe_pod_identities`.

**Recommendation for phase 3:** add one method that returns a small `Copy`
struct, instead of one trait method per flag. `SubstrateAdapter` already has
41 methods (`crates/libs/api/src/controllers/substrate/mod.rs:2136-2642`).
The reasons:

- The flags are static per substrate.
- Every `Substrate::Direct(_) => false` match becomes a field read.
- A phase 4 fake only has to fill in one value.

Questions that need the substrate to *do* something stay trait methods:
hydration and the lease store. Row-level questions belong on the shared enum,
because their crates (`sandbox-store`, `trigger-queue`, `internal-api`) cannot
depend on `api`.

```rust
/// What the active substrate does differently. One `const` per substrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubstrateCapabilities {
    /// Value of `sandboxes.managed_by` / `nodes.managed_by` this substrate owns
    /// (replaces `managed_by() -> &'static str` for callers that branch).
    pub managed_by: NodeManagedBy,
    /// Runs the pod-lifecycle reconciler set.
    pub owns_pod_lifecycle: bool,
    /// Restore claims `restoring` before the node call.
    pub claims_restoring_lifecycle: bool,
    /// `started` promises a routable execution row.
    pub has_pod_routability_contract: bool,
    /// How node-api targets are addressed.
    pub node_addressing: NodeAddressing,
    /// Nodes are registered through the admin API.
    pub registers_nodes_via_admin_api: bool,
    /// Cold creates read pool-configured shapes.
    pub uses_pool_configured_shapes: bool,
    /// Create requests may carry a node-pool placement pin (today's method).
    pub supports_node_pool_placement: bool,
}

pub trait SubstrateAdapter: Send + Sync {
    fn capabilities(&self) -> SubstrateCapabilities;
    // behaviour, not flags:
    async fn ensure_routable_execution(&self, id: SandboxId, budget: Duration)
        -> Result<ExecutionRoutability, SubstrateError> { Ok(ExecutionRoutability::Routable) }
    // …existing methods…
}
```

## 4. Phase 2 feasibility: one name for the predicate in Rust and SQL

### How `sql!` is defined

`crates/libs/common/core/src/sql_locator.rs` has two arms:

```rust
($sql:literal $(,)?) => { concat!("/*", module_path!(), ":", line!(), "*/ ", $sql) };   // &'static str
($sql:expr $(,)?)    => { format!("/*{}:{}*/ {}", module_path!(), line!(), $sql) };     // String
```

### How the `b`/`c` SQL is written

`python3 inventory.py sqlform …` classifies the SQL predicate lines in `b`/`c`
(both sides of the module):

```text
  57 sql!(literal)
  20 format!(runtime String)
   2 const &str (not wrapped)
format!(runtime String) crates/libs/api/src/controllers/substrate/direct.rs 2048
format!(runtime String) crates/libs/api/src/controllers/substrate/direct.rs 2160
const &str (not wrapped) crates/libs/api/src/routes/public/sandboxes.rs 900
const &str (not wrapped) crates/libs/api/src/routes/public/sandboxes.rs 918
format!(runtime String) crates/libs/cluster/controller/server/src/drained_node_terminator/store.rs 152
format!(runtime String) crates/libs/cluster/controller/server/src/drained_node_terminator/store.rs 192
format!(runtime String) crates/libs/cluster/controller/server/src/drained_node_terminator/store.rs 303
format!(runtime String) crates/libs/cluster/controller/server/src/failed_restorable_repair/store.rs 47
format!(runtime String) crates/libs/cluster/controller/server/src/orphaned_nodes/store.rs 82
format!(runtime String) crates/libs/cluster/controller/server/src/orphaned_nodes/store.rs 126
format!(runtime String) crates/libs/cluster/controller/server/src/started_sandbox_repair/store.rs 45
format!(runtime String) crates/libs/cluster/controller/server/src/suspended_execution_reaper/store.rs 64
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 144
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 160
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 457
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 480
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 494
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 529
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 569
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 664
format!(runtime String) crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs 682
format!(runtime String) crates/libs/common/svc/src/sql/sandbox_routability.rs 55
```

So 57 sit in `sql!("literal")` (static `&str`). 20 are in `format!` and reach
`sqlx` as `AssertSqlSafe(sql!(string))`. The remaining 2 are in a `const &str`
that is later `format!`ed (`PUBLIC_SANDBOX_SELECT`). Some predicates already
pass through Rust fragment builders: `routable_execution_or_non_k8s`,
`row_state_guards`, `*_k8s_finalization_sql`.

### Experiment

This ran in a throwaway crate outside the checkout. It depends on
`pplx-space-common-core` by path and on `sqlx 0.9`, and uses the repository's
pinned toolchain. Nothing from it is committed.

```rust
use pplx_space_common_core::sql;
const MANAGED_BY_K8S: &str = "managed_by = 'k8s'";            // candidate 1: a const
macro_rules! managed_by_k8s { () => { "managed_by = 'k8s'" }; } // candidate 2: a literal-producing macro

let today = sql!("SELECT id FROM sandboxes WHERE managed_by = 'k8s' ORDER BY id DESC LIMIT 1");
let a = sql!(concat!("SELECT id FROM sandboxes WHERE ", managed_by_k8s!(), " ORDER BY id DESC LIMIT 1"));
let b = sql!(format!("SELECT id FROM sandboxes WHERE {MANAGED_BY_K8S} ORDER BY id DESC LIMIT 1"));
let d_today = sql!("SELECT s.id\n         FROM sandboxes s\n         WHERE s.managed_by = 'k8s'\n           AND s.lifecycle = 'quarantined'");
let d = sql!(concat!("SELECT s.id\n         FROM sandboxes s\n         WHERE s.", managed_by_k8s!(), "\n           AND s.lifecycle = 'quarantined'"));
```

Output (`body` is the text after the `/*…*/ ` locator):

```text
today      type=&str "/*sqlexp:27*/ SELECT id FROM sandboxes WHERE managed_by = 'k8s' ORDER BY id DESC LIMIT 1"
A concat!  type=alloc::string::String "/*sqlexp:29*/ SELECT id FROM sandboxes WHERE managed_by = 'k8s' ORDER BY id DESC LIMIT 1"
B format!  type=alloc::string::String "/*sqlexp:31*/ SELECT id FROM sandboxes WHERE managed_by = 'k8s' ORDER BY id DESC LIMIT 1"
A body == today body: true
B body == today body: true
D body == D_today body: true
```

There are two compile-fail probes. One passes a `const` to `concat!` inside
`sql!`. The other passes the `concat!` form of `sql!` to `sqlx::query`:

```text
##### cargo build --bin fail_const        sql!(concat!("SELECT … WHERE ", MANAGED_BY_K8S))
error: expected a literal
  = note: only literals (like `"foo"`, `-42` and `3.14`) can be passed to `concat!()`
##### cargo build --bin fail_query        sqlx::query(sql!(concat!(…, managed_by_k8s!())))
error[E0277]: dynamic SQL strings should be audited for possible injections
    = help: the trait `SqlSafeStr` is not implemented for `std::string::String`
    = note: this trait is only implemented for `&'static str`, not all `&str` like the compiler error may suggest
```

The last test adds one arm to a local copy of `sql!`, placed between the two
existing arms:

```rust
(concat!($($part:expr),+ $(,)?) $(,)?) => {
    concat!("/*", module_path!(), ":", line!(), "*/ ", $($part),+)
};
```

```text
##### cargo run --bin extended_arm
today "/*extended_arm:15*/ SELECT id FROM sandboxes WHERE managed_by = 'k8s' ORDER BY id DESC LIMIT 1"
named "/*extended_arm:16*/ SELECT id FROM sandboxes WHERE managed_by = 'k8s' ORDER BY id DESC LIMIT 1"
bodies byte-identical: true
locator differs only in module path/line: "/*extended_arm:15*/" vs "/*extended_arm:16*/"
```

That version also passes `sqlx::query(named)` without `AssertSqlSafe`.

### Answer

- **A `const` cannot be used inside `sql!` and stay byte-identical *and*
  static.** `concat!` rejects consts. `format!` keeps the text byte-identical,
  but the result becomes a `String` that needs `AssertSqlSafe`.
- **A fragment that is a literal-producing `macro_rules!`** (exported from
  `pplx-space-common-core`, e.g. `managed_by_k8s!()`) works inside `concat!`.
  Today, though, `sql!(concat!(…))` takes the `expr` arm. The rendered text is
  byte-identical, but the type becomes `String`, so all 57 static call sites
  would have to change type.
- **The extra `concat!` arm keeps everything else identical.** Adding it to
  `sql!` makes `sql!(concat!(…, managed_by_k8s!(), …))` a `&'static str`. The
  rendered SQL is byte-identical apart from the locator, which also moves when
  the call site's line moves.
  - No call site uses `sql!(concat!(…))` today (`git grep -nE 'sql!\(\s*concat!'`
    finds none), so the extra arm changes no existing expansion.
  - The 20 `format!` sites can interpolate the same macro (`{}` with
    `managed_by_k8s!()`).
  - The aliased forms (`s.`, `n.`, `sandboxes.`) work as
    `concat!("s.", managed_by_k8s!())`.
  - `MANAGED_BY_ACTIVE` stays a bind (`managed_by = $n`). What phase 2 changes
    there is the bound value's type (below).

### The existing type for the substrate value

**`NodeManagedBy`** (`crates/libs/common/core/src/types/node.rs:684`; it is not
in `types/sandbox.rs`, which imports it at `:28`) already has what phase 2
needs:

- `Space` / `Kubernetes` variants;
- `const fn as_str()`, `parse`, `Display`, `FromStr`;
- `TryFrom<String>` / `From<…> for String`;
- serde through `String`;
- sqlx `Type`/`Encode`/`Decode` behind the `sqlx` feature;
- the Rust constants `NODE_MANAGED_BY_SPACE = "space"` and
  `NODE_MANAGED_BY_K8S = "k8s"`.

It is used for:

- `nodes.managed_by`, e.g. `common/core/src/types/admin.rs:38`,
  `api/src/routes/admin/node_store.rs:45`, `cluster/trigger-queue/src/lib.rs:40`;
- `NodeApiTarget.managed_by` (`api/src/controllers/substrate/mod.rs:1490`);
- the route identity's `substrate_kind` (`types/sandbox.rs:341,423`), which
  sbx-mgr, the gateway and the node-api sim also use.

It fits as the one name. What is missing:

1. **The name.** `Node…` reads as node-only, yet it already serves sandbox
   rows' route identity and is the natural type for `sandboxes.managed_by`. A
   `pub type SubstrateKind = NodeManagedBy;` alias, or a later rename, would
   fix this.
2. **SQL fragments.** The constants are unquoted `&str` and cannot feed
   `concat!`. Phase 2 needs exported literal macros (`managed_by_k8s!()`,
   `managed_by_space!()`, `not_managed_by_k8s!()`) plus the `concat!` arm of
   `sql!`.
3. **`SubstrateAdapter::managed_by()` returns `&'static str`**
   (`substrate/mod.rs:2141`). One caller parses it straight back into
   `NodeManagedBy`, with an error path that cannot fire
   (`transient_lifecycle_sweeper/store.rs:741-746`). Others compare strings:
   `admin/node_maintenance.rs:432`, `sandbox/queries.rs:606`,
   `routes/session_auth.rs:31-33`.
4. **`sandboxes.managed_by` is still a `String`** in the Rust row types:
   `api/src/models.rs:151`, `sandbox/store/src/lib.rs:121,470`,
   `api/src/routes/debug/response.rs:59,98`.

## 5. Test files that construct `DirectSubstrate`

`git grep -l DirectSubstrate` finds 22 files. 17 of them are test files: the
same 17 that `git grep -l DirectSubstrate -- '*test*' 'crates/**/tests/**'`
lists, so the count of 17 is confirmed. Every one is a `#[cfg(test)] mod`. The
other 5 are `substrate/mod.rs`, `substrate/direct.rs`, a comment in
`sandbox/lifecycle.rs:2378`, `infra/space/kind/README.md`, and an unrelated UI
variable.

| file | class | evidence | `SubstrateAdapter`/`Substrate` methods actually reached through Direct |
|---|---|---|---|
| `api/src/controllers/substrate/direct/tests.rs` | about Direct | calls Direct internals, e.g. `pool_resource_value` (:194), `direct_create_rejects_node_pool_placement` (:778); sim worker via `start_sim_worker` (:1184) | (subject under test) |
| `api/src/controllers/substrate/runtime_rpc/tests.rs` | about substrate layer | `peer_errors_are_returned_without_replaying_the_request` (:18) pins `RuntimeClient::call` retry behaviour | none |
| `api/src/fork_operation_executor/tests.rs` | about Direct | `direct_fork_copy_fallback_requires_a_fresh_ready_node_status` (:453) calls `select_ready_worker_node_api_target` on Direct `node_direct` snapshots | `select_ready_worker_node_api_target` |
| `api/src/cluster_admission/tests.rs` | needs any substrate | `empty_cluster_rejects_activation` (:690); one test (:745) does depend on Direct's stale-heartbeat count | `Substrate::eligible_start_node_count` |
| `api/src/controllers/sandbox/ingress/tests.rs` | needs any substrate | `stored_ingress_access_resumes_paused_execution_before_reporting_ready` (:259), sim worker | `wake_execution` |
| `api/src/controllers/sandbox/spaced/tests.rs` | needs any substrate | `reconnect_node_api_is_blocked_by_access_drain_gate` (:236) returns before the substrate is used | none |
| `api/src/controllers/sandbox/updates/postgres_tests.rs` | needs any substrate | `update_persists_metadata_and_reads_it_back` (:84), metadata-only branch | none |
| `api/src/controllers/sandbox/updates/transport_tests.rs` | needs any substrate | `untransmitted_update_is_replayed_once_on_a_fresh_client` (:316); moves the worker by rewriting `node_direct` (:273) | `probe_runtime_client` |
| `api/src/controllers/sandbox/lifecycle/tests.rs` | needs any substrate | `exhausting_the_finalize_retry_budget_stays_retryable` (:702) | `start_execution`, `delete_execution`, `runtime_client`, `eligible_start_node_count` |
| `api/src/controllers/sandbox/lifecycle/characterization_tests.rs` | needs any substrate | header (:8-12) says Direct is used for lack of an injectable seam; `unreachable_runtime_fails_actions_without_writing_lifecycle` (:417) | `start_execution`, `complete_activation_lease`, `wake_execution`, `suspend_execution`, `runtime_client`, `reachable_runtime_client`, `delete_observed_execution`, `eligible_start_node_count` |
| `api/src/controllers/sandbox/queries/create_postgres_tests.rs` | needs any substrate | `postgres_cold_federated_create_is_atomic_with_grant_upsert` (:1502); already has in-process trait fakes (:628, :1046) | `managed_by`, `pool_eligible_for_request`, `pool_supports_request_env`, `cleanup_failed_pooled_prepare` → `rollback_pooled_execution_claim` |
| `api/src/public_sandbox_operations/tests.rs` | needs any substrate | `existing_rows_are_classified_exhaustively_through_executor` (:415) | `managed_by`, `create_execution`, `admitted_pool_cleanup_pending` |
| `api/src/controllers/template/tests.rs` | needs any substrate | `system_build_withdraws_its_publication_grant_when_dispatch_fails` (:665) | `builder_client` |
| `api/src/controllers/substrate/kubernetes/tests.rs` | needs any substrate | `rejecting_admission` (:5931): Direct is only admission's empty capacity source (comment :9539) | `eligible_start_node_count` |
| `cluster/controller/server/src/public_operation_dispatcher/tests.rs` | needs any substrate | `native_executor_requeues_create_when_cluster_admission_rejects` (:656) | `managed_by`, `eligible_start_node_count` |
| `cluster/controller/server/src/transient_lifecycle_sweeper/lifecycle_writer_tests.rs` | needs any substrate | module doc (:17-21): Direct because it needs no node or Kubernetes API | `managed_by` (bound as `MANAGED_BY_ACTIVE`) |
| `cluster/controller/server/src/warm_pool/tests.rs` | needs any substrate | `reconcile_once_starts_preparing_republished_exact_release` (:1248); Direct only fills a field | none |

**3 files are about Direct or the substrate; 14 need any substrate**, and 3 of
those 14 reach no substrate method at all. For the phase 4 in-process fake,
the methods to support, with the number of files that reach each, are:

| method | files |
|---|---:|
| `eligible_start_node_count` | 5 |
| `managed_by` | ≥ 4 |
| `start_execution`, `runtime_client`, `wake_execution` | 2 each |
| `delete_execution`, `complete_activation_lease`, `suspend_execution`, `reachable_runtime_client`, `delete_observed_execution`, `probe_runtime_client`, `builder_client`, `pool_eligible_for_request`, `pool_supports_request_env`, `cleanup_failed_pooled_prepare`/`rollback_pooled_execution_claim`, `create_execution`, `admitted_pool_cleanup_pending` | 1 each |

That is 17 of the 41 trait methods plus one inherent method. Constraints on the
fake:

- **It has to stand in for `Substrate`, not only the trait.** Admission calls
  the inherent `Substrate::eligible_start_node_count`, and the four
  variant-match sites in §3 need an answer.
- **The `RuntimeClient` it returns must reach a worker.** It can point at the
  existing sim worker or be an in-process client. The ingress, transport,
  lifecycle and characterization suites use a live sim worker.
- **Four tests depend on Direct's own database behaviour**, so they belong in
  the Direct suite:
  - `characterization_tests.rs:648`, `:763`;
  - `cluster_admission/tests.rs:745`;
  - `transport_tests.rs:273`.
- **Outside the 17, about 16 more test files get Direct indirectly** through
  `AppState::from_pool` / `SandboxController::from_env`, because
  `Substrate::from_env` falls back to Direct. They are found with
  `git grep -lE "AppState::from_pool|from_pools_with_lifecycle|SandboxController::from_env" -- crates`.
  They are not traced here.

## 6. Direct-only tables: `node_direct`, `sandbox_direct_route_identities`, `sandbox_direct_route_provenance`

**Counts.** Inside the substrate module, only `direct.rs` names the tables:
38 code lines. `direct/` holds only `tests.rs`. Outside the module there are
**34 code lines in 14 production files**, counted with the word-boundary regex
over the scope in §1:

| file | code lines | role |
|---|---:|---|
| `api/src/controllers/sandbox/responses.rs` | 4 | join: `SANDBOX_JOIN_TAIL` effective node `COALESCE(direct_route.current_node_id, s.active_node_id)` (:74, :78); observed-node joins (:440, :528) |
| `api/src/controllers/sandbox/spaced.rs` | 1 | join/read: `fetch_authoritative_route_sandbox` (:795) merges Direct route and `sandbox_k8s_executions`: **belongs behind the substrate** |
| `api/src/public_template_operations.rs` | 1 | join: live-node filter (:200) |
| `api/src/routes/admin/node_maintenance.rs` | 7 | read `fetch_node` (:658), `fetch_nodes` (:687, the SQL fallback when `list_inventory_nodes` returns `None`); **write** `create_node` (:1032 read-CTE, :1066 UPDATE, :1137 INSERT), `update_node` (:1269 UPDATE), `hard_delete_direct_node` (:1349): the Direct node registry, **belongs behind Direct** |
| `api/src/routes/admin/node_store.rs` | 1 | join: `ADMIN_NODE_SELECT` (:294) |
| `api/src/routes/admin/templates.rs` | 1 | join: live-node filter (:525) |
| `api/src/routes/public/sandboxes.rs` | 1 | join: effective node for observed-state and failure joins (:943) |
| `cluster/controller/server/src/node_drain/store.rs` | 1 | join/read: node-api endpoint resolution in `promote_local_snapshot` (:2065), **move candidate** |
| `cluster/controller/server/src/tombstone_retention.rs` | 2 | join inside `DELETE FROM nodes` (:90), COUNT for a metric (:190): tombstone cleanup |
| `cluster/trigger-queue/src/lib.rs` | 1 | join/read: Direct endpoint + live-node filter (:240); crate cannot depend on `api` |
| `internal/api/server/src/data_plane_sessions.rs` | 7 | joins for route resolution (:872, :877, :1217, :1222), joins inside an UPDATE of `data_plane_sessions` (:917, :922), and an **error string** (:1010) |
| `internal/api/server/src/lib.rs` | 1 | read: `fetch_nodes` for `ListNodes` (:1215), a Direct-only near-copy of `node_maintenance.rs:687` |
| `internal/api/server/src/published_state.rs` | 4 | joins inside INSERT/UPDATE of observed state (:2244, :2306, :2349, :2423): a publish counts only from the effective node |
| `sandbox/store/src/lib.rs` | 2 | joins: `any_undescribed_artifact_owner` (:296), `auto_lifecycle_execution.has_direct_route` (:508) |

- **Only one place writes.** No production file outside `direct.rs` writes
  either route table or names `sandbox_direct_route_provenance`. The only
  writes outside the module are the admin node registry's writes to
  `node_direct`.
- **Two repeated shapes would remove most mentions.** A shared fragment in
  `sandbox-store` or `portability` would do more for them than adapter methods:
  - the effective-node `COALESCE(direct_route.current_node_id, s.active_node_id)`,
    repeated 12 times in 6 files;
  - the live-node filter `n.managed_by != 'space' OR (nd.node_id IS NOT NULL AND nd.deleted_at IS NULL)`,
    repeated 3 times.

**Precision notes for the sibling archcheck rule:**

- **Use word boundaries.** Match
  `\b(node_direct|sandbox_direct_route_identities|sandbox_direct_route_provenance)\b`.
  The bare substring also matches the constraint-name string
  `node_direct_metadata_is_object` (`api/src/routes/admin/node_maintenance.rs:357`),
  which takes that file from 7 lines to 8. No other identifier contains the
  names: no CamelCase types, singular `_identity` forms or function names.
- **Doc comments are excluded already.** The code-line rule drops them:
  `apps/admin/cli/src/cli.rs:181`, `common/core/src/types/admin.rs:510`,
  `sandbox/portability/src/lib.rs:490`, `sandbox/store/src/lib.rs:36`, plus
  one in `direct.rs`. Three of those files then have no code-line match.
- **One counted line is not SQL.** `internal/api/server/src/data_plane_sessions.rs:1010`
  is an error message, and the code-line rule counts it.
- **Migrations would overcount with substrings.** If the rule ever scans SQL
  migrations, substring matching hits index and constraint names
  (`idx_node_direct_*`, `node_direct_pkey`, `…_check`). Raw counts there:
  `db/postgres/20261002070625_baseline.up.sql` has 6/10/5 substring and 3/4/1
  word-boundary lines for the three tables; `ui/` has 0; `scripts/` has 2
  `node_direct`.

## 7. Recommended phase 1 ratchet

**Scope.** Match per archcheck code line of a production Rust file, the same
scope as `file_size`.

**Allowlisted paths** (not counted):

- `crates/libs/api/src/controllers/substrate/**`: the substrate module.
- Node-side code, where `NodeManagedBy` is the route identity's substrate:
  - `crates/libs/sbx/mgr/**`
  - `crates/libs/net/gateway/**`
  - `crates/apps/net/gateway/**`
  - `crates/libs/node/**`

  The `node/` entry also avoids an `x` hit, `component == "space"` in node-mgr.

**Patterns** (Rust `regex` syntax, from `RATCHET_PATTERNS` in `inventory.py`):

| key | regex | catches |
|---|---|---|
| `sql_managed_by` | `\bmanaged_by\s*(?:=\|<>\|!=)\s*(?:'(?:k8s\|space)'\|\$\d+)(?!\s*,)\|\bmanaged_by\s+IN\s*\(\s*'(?:k8s\|space)'` | SQL scoping and gating on `sandboxes`/`nodes` (b, c). `(?!\s*,)` skips `SET managed_by = …,` assignments, and the value list skips `kubernetes_pool_configs.managed_by = 'autoscaler'` |
| `rust_managed_by_str` | `(?:==\|!=)\s*"(?:k8s\|space)"\|^\s*"(?:k8s\|space)"\s*=>` | Rust string comparisons and match arms on a substrate string (d) |
| `substrate_variant` | `\bSubstrate::(?:Direct\|Kubernetes)\b` | matches on the api `Substrate` enum outside its module (d) |
| `node_managed_by_cmp` | `(?:==\|!=)\s*(?:Some\()?(?:[\w:]*::)?NodeManagedBy::(?:Space\|Kubernetes)\b` | typed comparisons (d) |

The Rust `regex` crate has no look-around, so the archcheck implementation
needs another way to express `(?!\s*,)`: match without it and drop matches
whose next non-space character is `,`.

**Precision at this commit.** The patterns hit 82 lines, and all 82 are `b`,
`c` or `d` rows of the CSV:

```text
archcheck file-level scope: 83 lines; with test-only items removed: 82 lines
  only in file-level scope: crates/libs/api/src/routes/public/sandboxes/admission_test_fixture.rs 166 SandboxController::new(Substrate::Kubernetes(Box::new(substrate)))
precision by category: {'d': 20, 'b': 29, 'c': 33}
  sql_managed_by: {'b': 28, 'c': 33}
  rust_managed_by_str: {'d': 5, 'b': 1}
  substrate_variant: {'d': 13}
  node_managed_by_cmp: {'d': 2}
```

**Baseline per file at this commit** (test-only items removed; archcheck's
file-level scope adds `crates/libs/api/src/routes/public/sandboxes/admission_test_fixture.rs` = 1):

| ratchet file | `sql_managed_by` | `rust_managed_by_str` | `substrate_variant` | `node_managed_by_cmp` | lines |
|---|---:|---:|---:|---:|---:|
| `crates/libs/api/src/controllers/sandbox/lifecycle.rs` |  |  | 2 |  | 2 |
| `crates/libs/api/src/controllers/sandbox/queries.rs` |  | 1 |  |  | 1 |
| `crates/libs/api/src/controllers/sandbox/spaced.rs` |  | 1 |  |  | 1 |
| `crates/libs/api/src/controllers/sandbox/wake_completion.rs` |  |  | 4 |  | 4 |
| `crates/libs/api/src/fork_operation_executor.rs` |  |  | 2 |  | 2 |
| `crates/libs/api/src/public_template_operations.rs` | 1 |  |  |  | 1 |
| `crates/libs/api/src/routes/admin/node_maintenance.rs` | 3 | 1 |  | 1 | 5 |
| `crates/libs/api/src/routes/admin/templates.rs` | 1 |  |  |  | 1 |
| `crates/libs/api/src/routes/public/sandboxes.rs` | 2 |  |  |  | 2 |
| `crates/libs/api/src/routes/session_auth.rs` |  | 1 |  |  | 1 |
| `crates/libs/cluster/controller/server/src/drained_node_terminator/store.rs` | 5 |  |  |  | 5 |
| `crates/libs/cluster/controller/server/src/failed_restorable_repair/store.rs` | 1 |  |  |  | 1 |
| `crates/libs/cluster/controller/server/src/lib.rs` |  |  | 1 |  | 1 |
| `crates/libs/cluster/controller/server/src/node_drain/store.rs` | 5 |  |  |  | 5 |
| `crates/libs/cluster/controller/server/src/orphaned_nodes/store.rs` | 2 |  |  |  | 2 |
| `crates/libs/cluster/controller/server/src/orphaned_restore_pod_gc/store.rs` |  | 1 |  |  | 1 |
| `crates/libs/cluster/controller/server/src/orphaned_sandboxes.rs` | 2 |  |  |  | 2 |
| `crates/libs/cluster/controller/server/src/orphaned_sandboxes/quarantine_recovery/store.rs` | 1 |  |  |  | 1 |
| `crates/libs/cluster/controller/server/src/reconcile_queue_gc/store.rs` | 2 |  |  |  | 2 |
| `crates/libs/cluster/controller/server/src/reconcilers.rs` |  |  | 2 |  | 2 |
| `crates/libs/cluster/controller/server/src/started_sandbox_repair/store.rs` | 2 |  |  |  | 2 |
| `crates/libs/cluster/controller/server/src/suspended_execution_reaper/store.rs` | 2 |  |  |  | 2 |
| `crates/libs/cluster/controller/server/src/tombstone_retention.rs` | 2 |  |  |  | 2 |
| `crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs` | 18 |  | 2 |  | 20 |
| `crates/libs/cluster/trigger-queue/src/lib.rs` | 1 |  |  | 1 | 2 |
| `crates/libs/common/svc/src/sql/sandbox_routability.rs` | 1 |  |  |  | 1 |
| `crates/libs/internal/api/server/src/lib.rs` | 4 |  |  |  | 4 |
| `crates/libs/internal/api/server/src/node_inventory.rs` | 1 |  |  |  | 1 |
| `crates/libs/internal/api/server/src/published_state.rs` | 3 |  |  |  | 3 |
| `crates/libs/internal/api/server/src/status.rs` | 1 |  |  |  | 1 |
| `crates/libs/internal/api/server/src/template_releases_store.rs` | 1 |  |  |  | 1 |
| `crates/libs/sandbox/store/src/lib.rs` |  | 1 |  |  | 1 |
| **total (32 files)** | 61 | 6 | 13 | 2 | 82 |

**What the patterns deliberately do not catch** (`b`/`c`/`d` rows outside the
module that the ratchet does not match):

```text
  b  44  KubernetesSubstrate (typed dependency)
  b  30  MANAGED_BY_ACTIVE
  b   1  MANAGED_BY_K8S
  b   1  TriggerSubstrate
  b  37  cfg(feature = "kubernetes")
  c  23  MANAGED_BY_K8S (callers)
  c   7  NOT_MANAGED_BY_K8S
  d   2  (none: response label)
  d   2  claims_restoring_lifecycle
  d   2  has_pod_routability_contract
  d   1  node_addressing
  d   6  registers_nodes_via_admin_api
  total 156
```

Why each is left out:

- **Typed `KubernetesSubstrate` dependencies (44) and `cfg(feature =
  "kubernetes")` gates (37)** are the structural half of Kubernetes-only
  reconcilers. They would cost:
  - for `\b(DirectSubstrate|KubernetesSubstrate)\b`: 44 lines, all legitimate
    typed deps;
  - for `feature\s*=\s*"kubernetes"`: 61 lines outside the module. 37 are
    structural `b`, 5 are match-arm gates already caught as
    `substrate_variant`, 9 are trace fields (`e`), and 5 are node-ctl's own
    feature (`f`). The last 5 are `x`: 4 for the node-api client's endpoint
    discovery feature and 1 for `common-core`'s pod-rendering gate.

  So 19 of the 61 are not substrate scoping at all, and the other 42 duplicate
  something already counted or are compile-time structure. If phase 1 wants a
  ratchet on these, make it a separate, advisory rule.
- **Helper call sites:** `*_k8s_finalization_sql` (23) and the 7 callers of
  `routable_execution_or_non_k8s`. These are calls of already-named
  predicates. The ratchet counts their definitions (`sandbox_routability.rs:55`)
  instead.
- **The Rust half of `MANAGED_BY_ACTIVE`:** the 30 `let managed_by =
  self.substrate.managed_by()` / `.bind(managed_by)` lines. The SQL side of
  each statement is already counted.
- **Follow-on lines of an already-counted decision site:** the `#[cfg]` line
  of a match arm, the 5 `require_direct_substrate(&state)` calls, and the
  `ConsoleSubstrate` match header. Every one of the 14 `d` sites has at least
  one counted line.

**False-positive cost: none at this commit.** The `(?!\s*,)` and value-list
restrictions avoid the only two `x` candidates (`cluster/pool/src/store.rs:332`
and the `autoscaler` predicates). The main future risk is
`rust_managed_by_str` matching a non-substrate `"space"` comparison in shared
code, such as a namespace or a path component, which is why `node/` is
allowlisted. A per-file cap keeps that local.

## 8. Findings (not fixed)

**Dead or always-true checks**

1. **Always-true predicates.** `nodes.managed_by IN ('space', 'k8s')` at
   `crates/libs/internal/api/server/src/lib.rs:1445` and
   `crates/libs/internal/api/server/src/template_releases_store.rs:124`. The
   column is `NOT NULL` with `CHECK (managed_by = ANY (ARRAY['space','k8s']))`
   (`db/postgres/20261002070625_baseline.up.sql:252,263`), so the filter never
   removes a row.
2. **Statements that can never match on Direct.** These sweeper statements
   bind `managed_by = $n` (the active substrate) *and* hard-code
   `managed_by = 'k8s'`:
   - `crates/libs/cluster/controller/server/src/transient_lifecycle_sweeper/store.rs:91-92`
     (`REPAIR_FAILED_ACTIVE_EXECUTION_SQL`)
   - `:1166-1167` (`list_failed_active_executions`)
   - `:1213-1214` (`note_failed_cleanup_attempt`)

   On Direct they run but match nothing; on Kubernetes the bind is redundant.
   The same file mixes two scoping styles (`MANAGED_BY_ACTIVE` and
   `MANAGED_BY_K8S`). By its own doc comment (`:939-945`), the
   `Substrate::Direct(_) => Routable` arm at `:952` is never reached in
   practice.
3. **A redundant write.** `crates/libs/api/src/routes/admin/node_maintenance.rs:1050`
   writes `SET managed_by = 'space'` on a row the candidate CTE already
   restricted to `n.managed_by = 'space'` (`:1035`).

**Writers that disagree**

4. **Node-reported `failed` is k8s-only.** In
   `crates/libs/internal/api/server/src/published_state.rs`
   (`publish_sandbox_states`), node-reported `failed` sets
   `public.sandboxes.lifecycle = 'failed'` only for `managed_by = 'k8s'` rows
   (`:2984`). In the same handler, node-reported `stopped` converges for every
   substrate (`:2944`, no `managed_by` filter), and `started` converges for
   both through `routable_execution_or_non_k8s` (`:2834`). No comment explains
   why Direct rows ignore a node-reported failure.
5. **Direct-only cleanup lives in shared handlers.** The Direct-only
   counterparts sit in shared internal-api handlers, not behind the substrate:
   - hard delete of absent `stopped` rows (`published_state.rs:2186`);
   - mark `deleted` when a node stops reporting a sandbox
     (`crates/libs/internal/api/server/src/status.rs:49`).

   Phase 3 would have to give these an owner.
6. **Row checks and substrate checks are mixed.** Some sites test the *row's*
   `managed_by`, others the *active* substrate:
   - row: `crates/libs/api/src/controllers/sandbox/spaced.rs:75`,
     `crates/libs/sandbox/store/src/lib.rs:485`,
     `crates/libs/cluster/controller/server/src/orphaned_restore_pod_gc/store.rs:23`;
   - active: `wake_completion.rs:95,256`, `lifecycle.rs:366`,
     `fork_operation_executor.rs:61`.

   The sweeper scopes everything to `MANAGED_BY_ACTIVE`, as if a database could
   hold rows of both substrates. Where it does, these two kinds of check
   disagree. Phase 3 has to decide whether mixed rows are possible before
   turning row checks into capabilities.

**Stringly-typed substrate identity**

7. **`managed_by()` returns a string.** `SubstrateAdapter::managed_by() ->
   &'static str` (`crates/libs/api/src/controllers/substrate/mod.rs:2141`) is
   not the typed `NodeManagedBy`. The consequences:
   - the sweeper parses the string back, with an unreachable error
     (`transient_lifecycle_sweeper/store.rs:741-746`);
   - `ConsoleSubstrate::from_managed_by` maps every non-`"k8s"` string to
     `Space` (`crates/libs/api/src/routes/session_auth.rs:31-35`);
   - `sandboxes.managed_by` is a `String` in `api/src/models.rs:151` and
     `sandbox/store/src/lib.rs:121,470`.
8. **An unknown node defaults to Space.** `delete_observed_sandbox` defaults an
   observed node's unknown `managed_by` to `Space`, whatever the active
   substrate (`crates/libs/api/src/controllers/sandbox/lifecycle.rs:1216-1220`).
9. **The premise about sbx-mgr's `NodeManagedBy` is inaccurate.** The task
   describes it as "a separate, node-side notion". It is the same
   `pplx-space-common-core` type that types control-plane `nodes.managed_by`
   (`types/node.rs:684`, `types/admin.rs:38`) and `NodeApiTarget`. sbx-mgr
   uses it for the route identity's `substrate_kind` and the
   `SPACE_NODE_MANAGED_BY` environment value. So phase 2's "one name" choice
   affects node-side code too, and the ratchet must allowlist node-side paths
   rather than the type.

**Repetition and naming**

10. **The same predicates are copied.**
    - `current_max_sandbox_id`, k8s-scoped, appears three times:
      `cluster/controller/server/src/orphaned_sandboxes.rs:1118`,
      `started_sandbox_repair/store.rs:280`,
      `suspended_execution_reaper/store.rs:111`.
    - The live-node filter appears three times:
      `cluster/trigger-queue/src/lib.rs:244`,
      `api/src/public_template_operations.rs:203`,
      `api/src/routes/admin/templates.rs:528`.
    - The Direct node list is duplicated:
      `internal/api/server/src/lib.rs:1218`,
      `api/src/routes/admin/node_maintenance.rs:687`.
11. **`managed_by` has a second meaning.** It is also the warm-pool config
    owner (`kubernetes_pool_configs.managed_by`, `PoolConfigOwner`), including
    a metric tag `managed_by` (`crates/libs/metrics/src/warm_pool.rs:489,514`).
    That accounts for 32 of the 263 `managed_by` rows outside the module. Any
    future rule that matches bare `managed_by` will hit them.
12. **Phase 4 coverage gap.** The lifecycle-writer characterization tests use
    Direct (`transient_lifecycle_sweeper/lifecycle_writer_tests.rs`), so the
    `managed_by = 'k8s'` writers in finding 2 cannot be reached through that
    sink.
13. **Ungated Kubernetes arms (consistent with the manifest).**
    `cluster/controller/server/src/reconcilers.rs:394` and `lib.rs:145` match
    `Substrate::Kubernetes` without a `cfg` gate. This matches the manifest
    comment that a `--no-default-features` cluster-controller build "does not
    compile yet". It is noted because phase 3 must keep or lift that
    constraint deliberately.

## Self-check

Every count in §§1–3 and §7 is generated output, pasted from `inventory.py`. As
an independent check, this standalone snippet recomputes the headline numbers
from the committed CSV alone. It also asserts that the `counts` tables appear
verbatim in this README:

```python
# Run from the repository root. Recomputes the README's tables from the CSV
# (and the CSV rows' source lines) and fails on any mismatch.
import csv, re, tomllib
from collections import Counter, defaultdict
D = "reports/direct-substrate-inventory/"
rows = list(csv.DictReader(open(D + "inventory.csv", newline="")))
readme = open(D + "README.md").read()
src = {}
def line(r):
    if r["file"] not in src:
        src[r["file"]] = open(r["file"]).read().split("\n")
    return src[r["file"]][int(r["line"]) - 1]
def md_table(title):
    """Rows of the README table whose first header cell is `title`."""
    lines = readme.split("\n")
    i = next(k for k, l in enumerate(lines) if l.startswith(f"| {title} |"))
    head = [c.strip() for c in lines[i].strip("|").split("|")]
    out = {}
    for l in lines[i + 2:]:
        if not l.startswith("|"):
            break
        cells = [c.strip() for c in l.strip("|").split("|")]
        out[cells[0]] = dict(zip(head[1:], cells[1:]))
    return out
def check(title, computed):
    t = md_table(title)
    assert set(t) == set(computed), (title, set(t) ^ set(computed))
    for k, c in computed.items():
        for col, v in t[k].items():
            want = sum(c.values()) if col == "total" else c.get(col, 0)
            assert str(want) == v, (title, k, col, v, want)
    print(f"ok  {title}: {len(t)} rows")
cats = Counter(); scope = defaultdict(Counter); mb = defaultdict(Counter)
crate = defaultdict(Counter); cap = defaultdict(Counter); fil = defaultdict(Counter)
members = tomllib.load(open("Cargo.toml", "rb"))["workspace"]["members"]
pkg = {m: tomllib.load(open(f"{m}/Cargo.toml", "rb"))["package"]["name"] for m in members}
for r in rows:
    c, ins = r["category"], r["in_substrate_module"] == "yes"
    scope["inside substrate module" if ins else "outside substrate module"][c] += 1
    scope["all"][c] += 1
    if "managed_by" in line(r):
        mb["inside" if ins else "outside"][c] += 1
    crate[pkg[max((m for m in members if r["file"].startswith(m + "/")), key=len)]][c] += 1
    if c in "bcd":
        fil[r["file"]][c] += 1
        if r["capability"]:
            cap[r["capability"]][c] += 1
check("scope", scope)
check("`managed_by` rows", mb)
check("crate", crate)
check("capability / predicate", cap)
top = dict(sorted(fil.items(), key=lambda kv: (-sum(kv[1].values()), kv[0]))[:12])
check("file (b+c+d)", top)
# Ratchet baseline: the patterns applied to the CSV rows' source lines.
R = {"sql_managed_by": r"\bmanaged_by\s*(?:=|<>|!=)\s*(?:'(?:k8s|space)'|\$\d+)(?!\s*,)|\bmanaged_by\s+IN\s*\(\s*'(?:k8s|space)'",
     "rust_managed_by_str": r"(?:==|!=)\s*\"(?:k8s|space)\"|^\s*\"(?:k8s|space)\"\s*=>",
     "substrate_variant": r"\bSubstrate::(?:Direct|Kubernetes)\b",
     "node_managed_by_cmp": r"(?:==|!=)\s*(?:Some\()?(?:[\w:]*::)?NodeManagedBy::(?:Space|Kubernetes)\b"}
allow = re.compile(r"^crates/(libs/api/src/controllers/substrate/|libs/sbx/mgr/|libs/net/gateway/|apps/net/gateway/|libs/node/)")
per = defaultdict(Counter); hitcats = Counter()
for r in rows:
    if allow.search(r["file"]):
        continue
    ps = [k for k, p in R.items() if re.search(p, line(r))]
    for k in ps:
        per[f"`{r['file']}`"][f"`{k}`"] += 1
    if ps:
        per[f"`{r['file']}`"]["lines"] += 1; hitcats[r["category"]] += 1
t = md_table("ratchet file")
total = t.pop(f"**total ({len(per)} files)**")
assert set(t) == set(per), set(t) ^ set(per)
for f, c in per.items():
    for col, v in t[f].items():
        assert v == (str(c[col]) if c[col] else ""), (f, col, v, c[col])
assert total["lines"] == str(sum(c["lines"] for c in per.values()))
assert set(hitcats) <= set("bcd"), hitcats
print(f"ok  ratchet baseline: {len(per)} files, {total['lines']} lines, categories {dict(hitcats)}")
# Headline numbers quoted in prose.
sqlpred = Counter(r["category"] for r in rows if r["in_substrate_module"] == "no" and r["category"] in "bc"
                  and re.search(r"\bmanaged_by\s*(=|<>|!=|IN)\s*(\('space',\s*'k8s'\)|'k8s'|'space'|\$\d+)", line(r))
                  and not line(r).strip().startswith(("let ", ".bind")))
facts = {"rows": len(rows), "d rows outside": scope["outside substrate module"]["d"],
         "managed_by rows": sum(mb["outside"].values()) + sum(mb["inside"].values()),
         "SQL b/c predicates outside": sum(sqlpred.values()),
         "x managed_by PoolConfigOwner": sum(1 for r in rows if r["category"] == "x" and "managed_by" in line(r)
                                             and "runtime/manager/cli" not in r["file"])}
assert facts == {"rows": 697, "d rows outside": 33, "managed_by rows": 299,
                 "SQL b/c predicates outside": 61, "x managed_by PoolConfigOwner": 32}, facts
print("ok  prose numbers:", facts)
```

```text
ok  scope: 3 rows
ok  `managed_by` rows: 2 rows
ok  crate: 27 rows
ok  capability / predicate: 23 rows
ok  file (b+c+d): 12 rows
ok  ratchet baseline: 32 files, 82 lines, categories {'d': 20, 'b': 29, 'c': 33}
ok  prose numbers: {'rows': 697, 'd rows outside': 33, 'managed_by rows': 299, 'SQL b/c predicates outside': 61, 'x managed_by PoolConfigOwner': 32}
```
