#!/usr/bin/env bash
# Regression test for ci/scripts/archcheck-pr-comment.sh against a fake `gh`
# that serves a paginated comment list and records what would be sent.
set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/archcheck-pr-comment.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

# Failures are recorded in a file, not a variable: assertions inside a
# command substitution, such as dry_run_body's, run in a subshell whose
# variables are discarded.
FAILURES="${WORK}/failures"
: >"${FAILURES}"
fail() {
    echo "FAIL: $*" >&2
    echo "$*" >>"${FAILURES}"
}

SHA=0123456789abcdef0123456789abcdef01234567
MARKER='<!-- archcheck-report -->'

mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/gh" <<'GH'
#!/usr/bin/env bash
# Each call is logged. A GET applies --jq to every page in FAKE_GH_PAGES, as
# `gh api --paginate --jq` does; anything else records its --input.
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_GH_LOG}"
method=GET
filter=.
input=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --method) method="$2"; shift ;;
        --jq) filter="$2"; shift ;;
        --input) input="$2"; shift ;;
    esac
    shift
done
if [ "${method}" = GET ]; then
    for page in "${FAKE_GH_PAGES}"/*.json; do
        [ -e "${page}" ] || continue
        jq -r "${filter}" "${page}"
    done
else
    cp "${input}" "${FAKE_GH_SENT}"
    echo '{"html_url": "https://github.com/example/repo/pull/7#issuecomment-1"}' | jq -r "${filter}"
fi
GH
chmod +x "${WORK}/bin/gh"
export PATH="${WORK}/bin:${PATH}"
export FAKE_GH_LOG="${WORK}/gh.log"
export FAKE_GH_SENT="${WORK}/sent.json"
export FAKE_GH_PAGES="${WORK}/pages"

findings="${WORK}/findings.md"
cat >"${findings}" <<'REPORT'
### archcheck: 1 finding (1 advisory, 0 required; 3 production Rust files checked)

#### `file_size.no_crossing` (advisory)

- ``crates/a/src/we`ird.rs``: 1001 code lines, over the 1,000-line limit.
REPORT
clean="${WORK}/clean.md"
echo '### archcheck: no findings (3 production Rust files checked)' >"${clean}"

# Page 1 has only near misses: the marker from a person, and a bot comment
# without it. Page 2 has the report comment.
other_comments='[
  {"id": 101, "user": {"login": "someone"}, "body": "<!-- archcheck-report -->\nnot ours"},
  {"id": 102, "user": {"login": "github-actions[bot]"}, "body": "another bot comment"}
]'
report_comment='[{"id": 222, "user": {"login": "github-actions[bot]"}, "body": "<!-- archcheck-report -->\nold"}]'
comments() {
    rm -rf "${FAKE_GH_PAGES}" "${FAKE_GH_SENT}"
    : >"${FAKE_GH_LOG}"
    mkdir -p "${FAKE_GH_PAGES}"
    echo "${other_comments}" >"${FAKE_GH_PAGES}/1.json"
    if [ "$1" = existing ]; then
        echo "${report_comment}" >"${FAKE_GH_PAGES}/2.json"
    fi
}

run() {
    bash "${SCRIPT}" --repo example/repo --pr 7 --sha "${SHA}" "$@"
}

# Prints the body a dry run would send, after checking its request line.
dry_run_body() {
    local expected="$1"
    shift
    local output
    output="$(run "$@" --dry-run)" || fail "dry run failed: $*"
    [ "$(head -n 1 <<<"${output}")" = "dry-run: ${expected}" ] ||
        fail "dry run printed '$(head -n 1 <<<"${output}")', not '${expected}'"
    if grep -v -- '--paginate' "${FAKE_GH_LOG}" | grep -q .; then
        fail "a dry run sent a request: $(cat "${FAKE_GH_LOG}")"
    fi
    tail -n +2 <<<"${output}" | jq -r .body
}

# Findings, no comment yet: create one.
comments none
body="$(dry_run_body "POST repos/example/repo/issues/7/comments" --report "${findings}")"
[ "$(head -n 1 <<<"${body}")" = "${MARKER}" ] || fail "the created body does not start with the marker"
# shellcheck disable=SC2016 # Markdown backticks, not a command substitution
grep -qF -- '- ``crates/a/src/we`ird.rs``: 1001 code lines' <<<"${body}" ||
    fail "the created body lacks the report: ${body}"
grep -qF "Evaluated at \`${SHA}\` with this pull request's \`ARCHITECTURE.toml\` and baseline" <<<"${body}" ||
    fail "the created body lacks the footer: ${body}"

# Findings and a comment on the second page: update it.
comments existing
body="$(dry_run_body "PATCH repos/example/repo/issues/comments/222" --report "${findings}")"
grep -qF '1 finding (1 advisory' <<<"${body}" || fail "the updated body lacks the report"

# Clean with a comment: say so as of the head.
comments existing
body="$(dry_run_body "PATCH repos/example/repo/issues/comments/222" --report "${clean}")"
grep -qF "No findings as of \`${SHA}\`." <<<"${body}" || fail "the clean body is '${body}'"

# Clean without a comment: post nothing.
comments none
output="$(run --report "${clean}" --dry-run)" || fail "the clean run without a comment failed"
[[ "${output}" == *"nothing to post"* ]] || fail "the clean run without a comment printed '${output}'"
grep -v -- '--paginate' "${FAKE_GH_LOG}" | grep -q . && fail "the clean run without a comment sent a request"

# archcheck could not run, with a comment: say so as of the head, without the
# old findings or any error text.
RUN_URL=https://github.com/example/repo/actions/runs/42
comments existing
body="$(dry_run_body "PATCH repos/example/repo/issues/comments/222" --could-not-run --run-url "${RUN_URL}")"
[ "$(head -n 1 <<<"${body}")" = "${MARKER}" ] || fail "the could-not-run body does not start with the marker"
grep -qF '### archcheck could not run' <<<"${body}" || fail "the could-not-run body is '${body}'"
grep -qF "archcheck could not check \`${SHA}\`" <<<"${body}" || fail "the could-not-run body lacks the SHA: ${body}"
grep -qF "[job log](${RUN_URL})" <<<"${body}" || fail "the could-not-run body lacks the job log link: ${body}"
grep -qF 'code lines' <<<"${body}" && fail "the could-not-run body kept findings: ${body}"

# archcheck could not run, without a comment: post nothing.
comments none
output="$(run --could-not-run --run-url "${RUN_URL}" --dry-run)" || fail "the could-not-run run without a comment failed"
[[ "${output}" == *"nothing to post"* ]] || fail "the could-not-run run without a comment printed '${output}'"
grep -v -- '--paginate' "${FAKE_GH_LOG}" | grep -q . && fail "the could-not-run run without a comment sent a request"

# A real run sends exactly the dry run's request.
comments existing
expected="$(run --report "${findings}" --dry-run | tail -n +2)"
output="$(run --report "${findings}")" || fail "the real run failed"
[ "$(jq -c . "${FAKE_GH_SENT}")" = "$(jq -c . <<<"${expected}")" ] || fail "the real run sent a different body"
grep -qF -- "--method PATCH repos/example/repo/issues/comments/222 --input" "${FAKE_GH_LOG}" ||
    fail "the real run called: $(cat "${FAKE_GH_LOG}")"
[[ "${output}" == *"#issuecomment-1" ]] || fail "the real run printed '${output}'"

# An oversized report is cut on a line boundary below the comment limit.
comments none
huge="${WORK}/huge.md"
{
    head -n 4 "${findings}"
    for index in $(seq 1 2000); do
        echo "- \`crates/a/src/file${index}.rs\`: 1001 code lines, over the 1,000-line limit."
    done
} >"${huge}"
body="$(dry_run_body "POST repos/example/repo/issues/7/comments" --report "${huge}")"
[ "$(printf '%s' "${body}" | wc -c)" -le 65000 ] || fail "the oversized body has $(printf '%s' "${body}" | wc -c) bytes"
grep -qF '_Report truncated.' <<<"${body}" || fail "the oversized body has no truncation note"
if grep -- '^- ' <<<"${body}" | grep -qv ': 1001 code lines, over the 1,000-line limit\.$'; then
    fail "the oversized body ends with a partial line"
fi

# A comment list that cannot be read fails the run and sends nothing.
comments existing
echo 'not json' >"${FAKE_GH_PAGES}/3.json"
run --report "${findings}" >/dev/null 2>&1 && fail "an unreadable comment list did not fail the run"
grep -v -- '--paginate' "${FAKE_GH_LOG}" | grep -q . && fail "an unreadable comment list still sent a request"

# Bad arguments and a file that is not a report change nothing.
comments existing
not_report="${WORK}/not-report.md"
echo 'error: something else' >"${not_report}"
for args in \
    "--repo example/repo --pr 7 --sha ${SHA}" \
    "--repo example/repo --pr 7 --sha short --report ${findings}" \
    "--repo example/repo --pr 7x --sha ${SHA} --report ${findings}" \
    "--repo example --pr 7 --sha ${SHA} --report ${findings}" \
    "--repo example/repo --pr 7 --sha ${SHA} --report ${not_report}" \
    "--repo example/repo --pr 7 --sha ${SHA} --report" \
    "--repo example/repo --pr 7 --sha ${SHA} --could-not-run" \
    "--repo example/repo --pr 7 --sha ${SHA} --could-not-run --run-url https://github.com/other/repo/actions/runs/42" \
    "--repo example/repo --pr 7 --sha ${SHA} --could-not-run --run-url https://github.com/example/repo/actions/runs/42x" \
    "--repo example/repo --pr 7 --sha ${SHA} --could-not-run --run-url https://github.com/example/repo/actions/runs/42 --report ${findings}" \
    "--repo example/repo --pr 7 --sha ${SHA} --run-url https://github.com/example/repo/actions/runs/42 --report ${findings}"; do
    status=0
    # shellcheck disable=SC2086 # split the argument list on purpose
    bash "${SCRIPT}" ${args} >/dev/null 2>&1 || status=$?
    [ "${status}" -eq 2 ] || fail "'${args}' exited ${status}, not 2"
done
[ ! -s "${FAKE_GH_LOG}" ] || fail "bad arguments reached gh: $(cat "${FAKE_GH_LOG}")"

if [ -s "${FAILURES}" ]; then
    echo "archcheck-pr-comment tests: $(wc -l <"${FAILURES}") failed" >&2
    exit 1
fi
echo "archcheck-pr-comment tests passed"
