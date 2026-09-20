# shellcheck shell=bash
# Source this file; requires Bash, curl, and jq. Never changes caller shell options.
# Public version for callers and release tooling.
# shellcheck disable=SC2034
FLARES_CLIENT_VERSION=0.1.3

_flares_error() { printf '%s\n' "$*" >&2; return 1; }

_flares_request() (
    # Subshell keeps temporary variables and cleanup traps out of the caller.
    local method=$1 path=$2 data=$3 mutation=$4 key=${5-} query=${6-}
    local base=${FLARES_BASE_URL:-http://127.0.0.1:8000} token=${FLARES_API_TOKEN-}
    local timeout=${FLARES_TIMEOUT:-15} work status
    if ! command -v curl >/dev/null || ! command -v jq >/dev/null; then
        _flares_error 'Flares requires curl and jq'; return 1;
    fi
    case "$base" in http://*|https://*) ;; *) _flares_error 'Invalid API base URL'; return 1;; esac
    # Reject credentials, query strings, fragments, and control characters in the base URL.
    if [[ "$base" == *'?'* || "$base" == *'#'* || "$base" == *'@'* || "$base" =~ [[:space:][:cntrl:]] ]]; then
        _flares_error 'Invalid API base URL'; return 1
    fi
    if [[ -z "$token" || "$token" =~ [^\!-\~] ]]; then
        _flares_error 'API token must be nonempty printable ASCII without spaces'; return 1
    fi
    if ! jq -en --arg t "$timeout" '$t | tonumber | . > 0 and . <= 86400' >/dev/null 2>&1; then
        _flares_error 'Invalid timeout'; return 1
    fi
    if [[ -n "$key" && ( ${#key} -gt 200 || "$key" =~ [^\!-\~] ) ]]; then
        _flares_error 'Invalid idempotency key'; return 1
    fi
    work=$(mktemp -d) || return 1
    trap 'rm -rf -- "$work"' EXIT
    # Keep credentials out of curl's argument list. mktemp creates a private directory.
    printf 'Authorization: Bearer %s\nContent-Type: application/json\n' "$token" > "$work/headers"
    if [[ -n "$key" ]]; then printf 'Idempotency-Key: %s\n' "$key" >> "$work/headers"; fi
    local -a args=(--disable --silent --proto '=http,https' --max-time "$timeout"
        --request "$method" --header "@$work/headers" --output "$work/body" --write-out '%{http_code}')
    if [[ -n "$data" ]]; then
        if ! jq -se 'length == 1 and (.[0] | type == "object")' <<< "$data" >/dev/null 2>&1; then
            _flares_error 'Request must be a JSON object'; return 1
        fi
        printf '%s' "$data" > "$work/request"
        args+=(--data-binary "@$work/request")
    fi
    if [[ -n "$query" ]]; then path="$path?$query"; fi
    if ! status=$(curl "${args[@]}" --url "${base%/}$path" 2>/dev/null); then
        _flares_error 'API request failed or timed out; delivery may have occurred. Reuse the idempotency key or check issue state before retrying'
        return 1
    fi
    if [[ "$status" != 2[0-9][0-9] ]]; then
        _flares_error "API returned HTTP $status"; return 1
    fi
    if [[ "$path" == /metrics ]]; then cat "$work/body"; return 0; fi
    if ! jq -se --arg path "${path%%\?*}" --arg method "$method" --arg mutation "$mutation" '
        length == 1 and (.[0] |
            if $mutation == "true" then
                type == "object" and (.notification.status |
                    IN("pending", "sent", "failed", "unknown", "not_attempted"))
            elif $path == "/healthz" or $path == "/readyz" then
                type == "object" and (.status | type == "string")
            elif $method == "DELETE" then .deleted == true
            elif $path == "/v1/heartbeats" and $method == "GET" then type == "array"
            elif $path == "/v1/issues" then
                type == "object" and (.items | type == "array") and (.total | type == "number")
            else type == "object" and has("id")
            end)
        ' "$work/body" >/dev/null 2>&1; then
        _flares_error 'API returned an invalid response'; return 1
    fi
    cat "$work/body"
    printf '\n'
    if [[ "$mutation" == true ]] && jq -e '.notification.status == "failed"' "$work/body" >/dev/null 2>&1; then
        _flares_error 'Request saved, but notification delivery failed' || :
        return 2
    fi
)

flares_open_issue() {
    [[ $# -ge 1 ]] || { _flares_error 'Usage: flares_open_issue ID [--title TEXT] [--message TEXT] [--severity LEVEL] [--remind-every-seconds SECONDS] [--notify-on-resolution]'; return 1; }
    local id=$1 data option severity=warning reminder=null resolution=false
    local title='' message='' has_title=false has_message=false
    shift
    while [[ $# -gt 0 ]]; do
        option=$1
        case "$option" in
            --notify-on-resolution) resolution=true; shift; continue ;;
            --title|--message|--severity|--remind-every-seconds)
                [[ $# -ge 2 ]] || { _flares_error 'Missing option value'; return 1; }
                case "$option" in
                    --title) title=$2; has_title=true ;;
                    --message) message=$2; has_message=true ;;
                    --severity) severity=$2 ;;
                    --remind-every-seconds)
                        if ! jq -en --arg n "$2" '$n | select(test("^[0-9]+$")) | tonumber | . >= 1 and . <= 31536000' >/dev/null 2>&1; then
                            _flares_error 'Reminder interval must be between 1 and 31536000 seconds'; return 1
                        fi
                        reminder=$2 ;;
                esac
                shift 2 ;;
            *) _flares_error 'Unknown option for flares_open_issue'; return 1 ;;
        esac
    done
    case "$severity" in info|warning|critical) ;; *) _flares_error 'Invalid severity'; return 1 ;; esac
    data=$(jq -cn --arg id "$id" --arg title "$title" --arg message "$message" \
        --arg severity "$severity" --arg reminder "$reminder" \
        --argjson resolution "$resolution" --argjson has_title "$has_title" --argjson has_message "$has_message" \
        '{id:$id,severity:$severity,notify_on_resolution:$resolution}
        + (if $has_title then {title:$title} else {} end)
        + (if $has_message then {message:$message} else {} end)
        + (if $reminder != "null" then {remind_every_seconds:($reminder|tonumber)} else {} end)') || return 1
    _flares_request POST /v1/issues/open "$data" true
}
flares_alert() {
    [[ $# -ge 2 ]] || { _flares_error 'Usage: flares_alert TITLE MESSAGE [--severity LEVEL] [--group-key KEY] [--idempotency-key KEY]'; return 1; }
    local title=$1 message=$2 data option severity=warning group='' has_group=false key=''
    shift 2
    while [[ $# -gt 0 ]]; do
        option=$1
        case "$option" in
            --severity|--group-key|--idempotency-key)
                [[ $# -ge 2 ]] || { _flares_error 'Missing option value'; return 1; }
                case "$option" in
                    --severity) severity=$2 ;;
                    --group-key) group=$2; has_group=true ;;
                    --idempotency-key)
                        [[ -n "$2" ]] || { _flares_error 'Invalid idempotency key'; return 1; }
                        key=$2 ;;
                esac
                shift 2 ;;
            *) _flares_error 'Unknown option for flares_alert'; return 1 ;;
        esac
    done
    case "$severity" in info|warning|critical) ;; *) _flares_error 'Invalid severity'; return 1 ;; esac
    data=$(jq -cn --arg title "$title" --arg message "$message" --arg severity "$severity" \
        --arg group "$group" --argjson has_group "$has_group" \
        '{title:$title,message:$message,severity:$severity}
        + (if $has_group then {group_key:$group} else {} end)') || return 1
    _flares_request POST /v1/alerts "$data" true "$key"
}
flares_register_heartbeat() {
    [[ $# == 1 ]] || { _flares_error 'Usage: flares_register_heartbeat JSON'; return 1; }
    _flares_request POST /v1/heartbeats "$1" false
}
flares_close_issue() {
    [[ $# == 1 ]] || { _flares_error 'Usage: flares_close_issue ID'; return 1; }
    local data
    data=$(jq -cn --arg id "$1" '{id:$id}') || return 1
    _flares_request POST /v1/issues/close "$data" true
}
flares_check_in() {
    [[ $# == 1 ]] || { _flares_error 'Usage: flares_check_in ID'; return 1; }
    local data
    data=$(jq -cn --arg id "$1" '{id:$id}') || return 1
    _flares_request POST /v1/heartbeats/check-in "$data" false
}
flares_get_issue() {
    [[ $# == 1 ]] || { _flares_error 'Usage: flares_get_issue ID'; return 1; }
    local id
    id=$(jq -rn --arg id "$1" '$id|@uri') || return 1
    _flares_request GET /v1/issue '' false '' "id=$id"
}
flares_delete_heartbeat() {
    [[ $# == 1 ]] || { _flares_error 'Usage: flares_delete_heartbeat ID'; return 1; }
    local id
    id=$(jq -rn --arg id "$1" '$id|@uri') || return 1
    _flares_request DELETE /v1/heartbeat '' false '' "id=$id"
}
flares_list_issues() {
    [[ $# -le 3 ]] || { _flares_error 'Usage: flares_list_issues [STATUS] [LIMIT] [OFFSET]'; return 1; }
    local query
    query=$(jq -rn --arg status "${1-}" --arg limit "${2-100}" --arg offset "${3-0}" \
        '"limit="+($limit|@uri)+"&offset="+($offset|@uri)+(if $status=="" then "" else "&status="+($status|@uri) end)') || return 1
    _flares_request GET /v1/issues '' false '' "$query"
}
flares_get_delivery() {
    [[ $# == 1 && "$1" =~ ^[0-9]+$ ]] || { _flares_error 'Usage: flares_get_delivery NUMERIC_ID'; return 1; }
    _flares_request GET "/v1/deliveries/$1" '' false
}
flares_list_heartbeats() { _flares_request GET /v1/heartbeats '' false; }
flares_health() { _flares_request GET /healthz '' false; }
flares_readiness() { _flares_request GET /readyz '' false; }
flares_metrics() { _flares_request GET /metrics '' false; }
