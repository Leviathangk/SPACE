#!/usr/bin/env bash
set -euo pipefail
exec python3 "$TEST_SRCDIR/$TEST_WORKSPACE/ci/benchmarks/kind-bazel/pool.py" scenario "$@"
