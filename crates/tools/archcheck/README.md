# archcheck

`archcheck` checks repository structure rules declared in
[`ARCHITECTURE.toml`](../../../ARCHITECTURE.toml). Existing violations that a
rule tolerates for now are listed in
[`architecture-baseline.toml`](../../../architecture-baseline.toml).

```sh
cargo arch                     # human-readable report
cargo arch --format github     # GitHub Actions annotations
cargo arch --format markdown   # Markdown report
cargo arch --summary <path>    # also write the Markdown report to <path>
cargo arch --write-baseline    # regenerate architecture-baseline.toml
cargo arch --root <path>       # check another checkout
```

`cargo arch` is an alias in `.cargo/config.toml`. It builds into the
workspace target directory, wherever it is run from. A caller that must not
wait on the build lock of a concurrent workspace build, such as an editor hook,
can set `CARGO_TARGET_DIR` to a directory of its own. Without `--root`, the
tool checks the Cargo workspace that contains the current directory.

## Modes and exit status

Each behaviour of a rule has its own mode, so behaviours can graduate one at a
time:

- `advisory`: reported, never fails the check.
- `required`: reported and fails the check.

| Exit | Meaning |
| ---- | ------- |
| 0 | No required-mode findings (advisory findings only, or none). |
| 1 | At least one required-mode finding. |
| 2 | Configuration, usage or I/O error. |

Configuration fails closed. A missing `ARCHITECTURE.toml` or baseline,
unparseable TOML, an unknown key, section or mode, a baseline key that is not a
normalized repository-relative `.rs` path (no leading `./` or `/`, no `.` or
`..` segments, no empty segments), and a workspace `members` entry with glob
characters are all errors (exit 2), not skipped checks.

`--summary` writes the Markdown report whatever `--format` prints and does not
change the exit status: required findings still exit 1. A configuration error
writes no report, and a report that cannot be written exits 2.

## Output formats

- `human`: findings grouped by rule and behaviour, then a one-line summary.
- `github`: one workflow command per finding, then the summary. Advisory
  findings are `::warning` annotations and required findings are `::error`
  annotations. Each is attached to line 1 of the file, because GitHub may not
  show an annotation without a line on the file in a pull request's diff.
- `markdown`: a report for the CI job summary and pull request comments. A
  heading carries the summary; findings are grouped by rule and behaviour under
  a heading that names the mode, followed by a short footer. With no findings
  the report is the heading line alone.

On a pull request, file paths, workspace member names and TOML keys are chosen
by its author, and archcheck's output may land in a GitHub Actions log, where
the runner reads a line as a workflow command when it starts with `::` after
leading whitespace or contains `##[` anywhere. Every human-format line, the
summary line and every error on stderr therefore go through one escaping
function: control characters (including CR, LF and DEL) and line or paragraph
separators are escaped as `\n` or `\u{…}`, the `[` of `##[` is escaped, and so
is the first `:` of a leading `::`. Text from the tree stays on its own line
and never becomes a command. The `github` format escapes `%`, CR and LF in
messages, and also `:` and `,` in properties, as the workflow command syntax
requires.

The Markdown report also treats paths as untrusted. A path appears only inside
an inline code span whose backtick fence is longer than any backtick run in the
path. Control characters, line separators, Unicode format characters (general
category Cf, such as bidi controls, zero-width characters, U+00AD,
U+FFF9–U+FFFB and the tag characters U+E0000–U+E007F) and the `[` of `##[` are
escaped as `\n` or `\u{…}`, and paths longer than 200 characters are shortened
to their end. Other text is
backslash-escaped, and the report contains no raw HTML. At most 50 findings
are listed, followed by how many were left out.

## Definitions

- **Production Rust file**: a `.rs` file under `<member>/src/` for each
  workspace member listed in the root `Cargo.toml`, excluding test modules:
  files named `tests.rs` or ending in `_tests.rs` (the sibling-test convention
  `scripts/check_no_inline_rust_test_modules.py` uses), and every file below a
  directory named `tests` under `src/`, such as `src/<module>/tests/mod.rs`
  (the `*/tests/*` exclusion `scripts/check-rust-error-policy.sh` uses). The
  tool walks each member's `src/` directory on disk. Integration tests,
  benches, examples and build scripts are outside `src/` and are not counted.
- **Code line**: a line that, after trimming whitespace, is non-empty and does
  not start with `//`, so `///` and `//!` doc comments do not count either.
  Block comments and attributes do count. Comments and blank lines are free, so
  the only way under the limit is to restructure the file. A leading UTF-8 byte
  order mark is ignored.

## Rules

### `file_size`

```toml
[rules.file_size]
limit = 1000

[rules.file_size.no_crossing]
mode = "advisory"

[rules.file_size.listed_no_growth]
mode = "advisory"

[rules.file_size.stale_entries]
mode = "advisory"
```

- `no_crossing`: a production file that is not in the baseline has more code
  lines than `limit`.
- `listed_no_growth`: a file with a `[file_size]` baseline entry has more code
  lines than its cap: it grew.
- `stale_entries`: a `[file_size]` baseline entry can be tightened, because
  - the file is gone (deleted, renamed, or no longer a production file):
    remove the entry. A renamed file is then checked by `no_crossing` under its
    new path;
  - the file has fewer code lines than its cap but is still over the limit:
    lower the cap to the new count;
  - the file is at or under the limit: remove the entry.

`stale_entries` is separate from `listed_no_growth` so that requiring no growth
does not also fail the change that shrinks or splits a listed file.

## Baseline

`architecture-baseline.toml` is generated by `cargo arch --write-baseline`,
which rewrites it from the current tree without reading the old file. Each
rule that needs a baseline owns one table. `[file_size]` maps every production
file over the limit to its current code-line count, which becomes its cap.

Entries should only be removed or have their caps lowered. The
`stale_entries` findings say when an entry can be tightened.

## Adding a rule

1. Add a module under `src/rules/` with a type that implements `Rule`. Its
   findings carry the rule's name, which is also the name of its
   `[rules.<name>]` table.
2. Add the rule's configuration struct as a field of `RulesConfig` in
   `src/config.rs`, with one `BehaviourConfig` per behaviour, and add the table
   to `ARCHITECTURE.toml`.
3. If the rule tolerates existing violations, add a table to `Baseline` in
   `src/baseline.rs` and implement `Rule::update_baseline`.
4. Register the rule in `rules::registered()`.
5. Add tests in `tests/` that build temporary workspaces with `tempfile`.
   `tests/repository.rs` already runs every registered rule on this
   repository.
