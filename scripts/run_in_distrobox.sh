#!/usr/bin/env bash
# Run on the host. Connect and supervise the container process.
set -euo pipefail

nvstt_box="${NVSTT_DISTROBOX:-Fedora}"
nvstt_binary="${NVSTT_BINARY:-$HOME/.cargo/bin/nvstt}"
nvstt_bus="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is required}/bus}"
nvstt_job=""
nvstt_watchdog=""
nvstt_stopping=false
nvstt_stop_timeout="${NVSTT_STOP_TIMEOUT:-30}"
[[ "$nvstt_stop_timeout" =~ ^[0-9]+$ ]] && (( 10#$nvstt_stop_timeout >= 1 && 10#$nvstt_stop_timeout <= 30 )) || {
    printf "nvstt: NVSTT_STOP_TIMEOUT must be between 1 and 30 seconds\\n" >&2
    exit 1
}
nvstt_control="$(mktemp -d "${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is required}/nvstt-launch.XXXXXX")"

forward_signal() {
    local requested_signal="$1"
    # The shared PID belongs to this launch. Validate its environment before
    # signaling it, so a stale file cannot target an unrelated process.
    distrobox enter --name "$nvstt_box" -- bash -c '
        set -euo pipefail
        control="$1"
        [[ -d "/run/host$control" ]] && control="/run/host$control"
        [[ -f "$control/pid" ]] || exit 0
        read -r pid < "$control/pid"
        [[ "$pid" =~ ^[0-9]+$ && -r "/proc/$pid/environ" ]] || exit 0
        while IFS= read -r -d "" variable; do
            if [[ "$variable" == "NVSTT_LAUNCH_ID=$1" ]]; then
                kill -s "$2" "$pid"
                exit 0
            fi
        done < "/proc/$pid/environ"
    ' nvstt-signal "$nvstt_control" "$requested_signal" || true
}

stop_child() {
    nvstt_stopping=true
    : > "$nvstt_control/stop"
    forward_signal TERM
    if [[ -z "$nvstt_watchdog" ]]; then
        (
            trap - EXIT TERM INT
            sleep "$nvstt_stop_timeout" &
            timer="$!"
            trap 'kill "$timer" 2>/dev/null || true' EXIT
            wait "$timer"
            forward_signal KILL
        ) &
        nvstt_watchdog="$!"
    fi
}

cleanup() {
    if [[ -n "$nvstt_watchdog" ]]; then
        kill "$nvstt_watchdog" 2>/dev/null || true
        wait "$nvstt_watchdog" 2>/dev/null || true
    fi
    if [[ -n "$nvstt_job" ]]; then
        forward_signal TERM
        wait "$nvstt_job" 2>/dev/null || true
    fi
    gio trash "$nvstt_control" 2>/dev/null || true
}
trap stop_child TERM INT
trap cleanup EXIT

distrobox enter --name "$nvstt_box" -- env \
    NVSTT_DESKTOP_BUS="$nvstt_bus" NVSTT_CONTAINER_BINARY="$nvstt_binary" \
    NVSTT_LAUNCH_ID="$nvstt_control" \
    bash -c '
        set -euo pipefail
        address="$NVSTT_DESKTOP_BUS"
        case "$address" in
            unix:path=*)
                socket="${address#unix:path=}"
                socket="${socket%%,*}"
                if [[ ! -S "/run/host$socket" ]]; then
                    printf "nvstt: mapped desktop session bus socket is unavailable\\n" >&2
                    exit 1
                fi
                address="unix:path=/run/host${address#unix:path=}"
                ;;
        esac
        export DBUS_SESSION_BUS_ADDRESS="$address"
        control="$NVSTT_LAUNCH_ID"
        [[ -d "/run/host$control" ]] && control="/run/host$control"
        printf "%s\\n" "$$" > "$control/pid"
        [[ ! -f "$control/stop" ]] || exit 0
        exec "$NVSTT_CONTAINER_BINARY" "$@"
    ' nvstt "$@" &
nvstt_job="$!"
# A signal can interrupt wait. Wait again until the container command exits.
nvstt_result=0
while true; do
    if wait "$nvstt_job"; then break; else nvstt_result="$?"; fi
    if ! kill -0 "$nvstt_job" 2>/dev/null; then break; fi
done
nvstt_job=""
if "$nvstt_stopping"; then exit 0; fi
exit "$nvstt_result"
