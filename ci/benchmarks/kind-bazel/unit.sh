#!/usr/bin/env bash
set -euo pipefail
cd "$TEST_SRCDIR/$TEST_WORKSPACE"
export UV_PROJECT_ENVIRONMENT="$TEST_TMPDIR/venv"
exec uv --no-config run --frozen --python 3.11 --with pytest==8.4.2 \
  python -m pytest -c /dev/null -p no:cacheprovider "$1"
