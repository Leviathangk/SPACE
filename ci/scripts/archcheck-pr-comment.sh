#!/usr/bin/env bash
# Keep one comment on a pull request that lists its archcheck findings.
#
# usage: archcheck-pr-comment.sh --repo <owner/name> --pr <number> --sha <head sha>
#            --report <report.md> [--dry-run]
#
# <report.md> is `archcheck --format markdown` output for the pull request head.
# The comment is the first one by github-actions[bot] whose body starts with the
# marker below. With findings, it is created or updated in place. With none, an
# existing comment is edited to say so, and no comment is created.
#
# --dry-run still reads the comment list but prints the request it would send
# instead of sending it. Needs gh, authenticated through GH_TOKEN, and jq.
set -euo pipefail

MARKER='<!-- archcheck-report -->'
BOT_LOGIN='github-actions[bot]'
# GitHub rejects comment bodies over 65,536 characters; a byte count is a safe
# bound for that.
MAX_BODY_BYTES=65000
# A report without findings is this heading alone; see
# crates/tools/archcheck/src/report/markdown.rs.
CLEAN_HEADING='### archcheck: no findings'

usage() {
    sed -n '4,5p' "$0" | sed 's/^# //' >&2
    exit 2
}

die() {
    echo "archcheck-pr-comment: $*" >&2
    exit 2
}

repo=""
pr=""
sha=""
report=""
dry_run=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --repo | --pr | --sha | --report)
            [ "$#" -ge 2 ] || usage
            case "$1" in
                --repo) repo="$2" ;;
                --pr) pr="$2" ;;
                --sha) sha="$2" ;;
                --report) report="$2" ;;
            esac
            shift 2
            ;;
        --dry-run)
            dry_run=true
            shift
            ;;
        *) usage ;;
    esac
done

[[ "${repo}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "--repo must be owner/name"
[[ "${pr}" =~ ^[1-9][0-9]*$ ]] || die "--pr must be a pull request number"
[[ "${sha}" =~ ^[0-9a-f]{40}$ ]] || die "--sha must be a full commit SHA"
[ -f "${report}" ] || die "--report ${report} is not a file"
[[ "$(head -n 1 "${report}")" == '### archcheck: '* ]] || die "${report} is not an archcheck report"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# Collected in full rather than piped to `head`, which could fail a later page
# with SIGPIPE.
ids="$(
    gh api --paginate "repos/${repo}/issues/${pr}/comments?per_page=100" \
        --jq ".[] | select(.user.login == \"${BOT_LOGIN}\" and (.body | startswith(\"${MARKER}\"))) | .id"
)"
comment_id="${ids%%$'\n'*}"
if [ -n "${comment_id}" ] && ! [[ "${comment_id}" =~ ^[0-9]+$ ]]; then
    die "unexpected comment id ${comment_id}"
fi

footer="_Evaluated at \`${sha}\` with this pull request's \`ARCHITECTURE.toml\` and baseline. \
This comment is updated on every push._"

body="${work}/body.md"
if [[ "$(head -n 1 "${report}")" == "${CLEAN_HEADING}"* ]]; then
    if [ -z "${comment_id}" ]; then
        echo "archcheck-pr-comment: no findings and no earlier comment; nothing to post."
        exit 0
    fi
    {
        printf '%s\n' "${MARKER}"
        head -n 1 "${report}"
        # shellcheck disable=SC2016 # Markdown backticks, not a command substitution
        printf '\nNo findings as of `%s`.\n\n%s\n' "${sha}" "${footer}"
    } >"${body}"
else
    max_report_bytes=$((MAX_BODY_BYTES - ${#MARKER} - ${#footer} - 256))
    {
        printf '%s\n' "${MARKER}"
        if [ "$(wc -c <"${report}")" -le "${max_report_bytes}" ]; then
            cat "${report}"
        else
            # Drop the last, possibly cut, line.
            head -c "${max_report_bytes}" "${report}" | sed '$d'
            # shellcheck disable=SC2016 # Markdown backticks, not a command substitution
            printf '\n_Report truncated. Run `cargo arch` for every finding._\n'
        fi
        printf '\n%s\n' "${footer}"
    } >"${body}"
fi

if [ -n "${comment_id}" ]; then
    method=PATCH
    endpoint="repos/${repo}/issues/comments/${comment_id}"
else
    method=POST
    endpoint="repos/${repo}/issues/${pr}/comments"
fi
jq -n --rawfile body "${body}" '{body: $body}' >"${work}/request.json"

if [ "${dry_run}" = true ]; then
    echo "dry-run: ${method} ${endpoint}"
    cat "${work}/request.json"
    exit 0
fi
url="$(gh api --method "${method}" "${endpoint}" --input "${work}/request.json" --jq '.html_url')"
echo "archcheck-pr-comment: ${method} ${endpoint}: ${url}"
