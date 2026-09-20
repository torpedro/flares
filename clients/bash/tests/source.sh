#!/usr/bin/env bash
set -euo pipefail
trap ':' EXIT
before_options=$(set +o)
before_traps=$(trap -p)
# shellcheck source=clients/bash/flares.sh
source "${BASH_SOURCE[0]%/*}/../flares.sh"
[[ $(set +o) == "$before_options" ]]
[[ $(trap -p) == "$before_traps" ]]
export FLARES_API_TOKEN='invalid token'
if flares_health >/dev/null 2>&1; then
    printf 'Expected invalid token to fail\n' >&2
    exit 1
fi
[[ $(set +o) == "$before_options" ]]
[[ $(trap -p) == "$before_traps" ]]
