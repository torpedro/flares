#!/usr/bin/env bash
set -euo pipefail
trap ':' EXIT
before_options=$(set +o)
before_traps=$(trap -p)
# shellcheck source=clients/bash/flare.sh
source "${BASH_SOURCE[0]%/*}/../flare.sh"
[[ $(set +o) == "$before_options" ]]
[[ $(trap -p) == "$before_traps" ]]
export FLARE_API_TOKEN='invalid token'
if flare_health >/dev/null 2>&1; then
    printf 'Expected invalid token to fail\n' >&2
    exit 1
fi
[[ $(set +o) == "$before_options" ]]
[[ $(trap -p) == "$before_traps" ]]
