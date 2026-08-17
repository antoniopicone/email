#!/usr/bin/env bash
# Launch mailview on a virtual X display and capture its window.
#
#   tools/screenshot.sh OUTPUT.png [--scheme light|dark] [--wait SECONDS]
#                       [--size WxH] [--click SELECTOR] [-- ARGS...]
#
# Everything after `--` is passed through to the mailview binary.
set -uo pipefail

OUT="${1:?usage: screenshot.sh OUTPUT.png [--scheme light|dark] [--wait N] [-- app args]}"
shift

SCHEME="light"
WAIT=6
WIDTH=1680
HEIGHT=1050
KEYS=()
CLICKS=()
TYPE_TEXT=""
CAPTURE_ROOT=0
APP_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --scheme) SCHEME="$2"; shift 2 ;;
        --wait)   WAIT="$2";   shift 2 ;;
        --size)   WIDTH="${2%x*}"; HEIGHT="${2#*x}"; shift 2 ;;
        # Send a key combination (e.g. ctrl+Return) once the window is up.
        --key)    KEYS+=("$2"); shift 2 ;;
        # Click at X,Y inside the window, e.g. --click 100,217
        --click)  CLICKS+=("$2"); shift 2 ;;
        --type)   TYPE_TEXT="$2"; shift 2 ;;
        # Capture the whole screen, needed for secondary windows.
        --root)   CAPTURE_ROOT=1; shift ;;
        --)       shift; APP_ARGS=("$@"); break ;;
        *)        echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${MAILVIEW_BIN:-$REPO/target/debug/mailview}"
DISPLAY_NUM="${DISPLAY_NUM:-99}"
OUT="$(realpath -m "$OUT")"
mkdir -p "$(dirname "$OUT")"

# Detach every helper from this shell's stdout/stderr so the calling process
# never blocks waiting on an inherited pipe.
start_detached() {
    setsid "$@" </dev/null >>/tmp/mailview-harness.log 2>&1 &
    echo $!
}

cleanup() {
    [[ -n "${APP_PID:-}"  ]] && kill -TERM "-$APP_PID"  2>/dev/null
    [[ -n "${WM_PID:-}"   ]] && kill -TERM "-$WM_PID"   2>/dev/null
    [[ -n "${XVFB_PID:-}" ]] && kill -TERM "-$XVFB_PID" 2>/dev/null
    return 0
}
trap cleanup EXIT

pkill -f "Xvfb :$DISPLAY_NUM" 2>/dev/null
sleep 1

XVFB_PID=$(start_detached Xvfb ":$DISPLAY_NUM" -screen 0 "${WIDTH}x${HEIGHT}x24" -nolisten tcp)
export DISPLAY=":$DISPLAY_NUM"

display_ready() {
    [[ -S "/tmp/.X11-unix/X$DISPLAY_NUM" ]] || return 1
    command -v xdpyinfo >/dev/null 2>&1 || return 0
    xdpyinfo >/dev/null 2>&1
}

for _ in $(seq 1 60); do
    display_ready && break
    sleep 0.25
done
if ! display_ready; then
    echo "ERROR: Xvfb failed to start on :$DISPLAY_NUM" >&2
    tail -20 /tmp/mailview-harness.log >&2
    exit 1
fi

WM_PID=$(start_detached openbox)
sleep 1

# Software rendering: the virtual display has no GPU.
export GDK_BACKEND=x11
export GSK_RENDERER=cairo
export LIBGL_ALWAYS_SOFTWARE=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export GTK_A11Y=none
export MAILVIEW_FORCE_SCHEME="$SCHEME"

rm -f /tmp/mailview.log
APP_PID=$(setsid "$BIN" "${APP_ARGS[@]}" </dev/null >/tmp/mailview.log 2>&1 & echo $!)

# Wait for the window to be mapped.
WIN=""
for _ in $(seq 1 60); do
    WIN="$(xdotool search --onlyvisible --class 'mailview' 2>/dev/null | tail -1)"
    [[ -n "$WIN" ]] && break
    if ! kill -0 "$APP_PID" 2>/dev/null; then
        echo "ERROR: mailview exited during startup. Log:" >&2
        tail -40 /tmp/mailview.log >&2
        exit 1
    fi
    sleep 0.5
done

if [[ -z "$WIN" ]]; then
    echo "ERROR: mailview window never appeared. Log:" >&2
    tail -40 /tmp/mailview.log >&2
    exit 1
fi

# Let async work (folder load, WebKit paint) settle before capturing.
sleep "$WAIT"

xdotool windowactivate "$WIN" >/dev/null 2>&1
xdotool windowraise "$WIN" >/dev/null 2>&1
sleep 1

for point in "${CLICKS[@]:-}"; do
    [[ -z "$point" ]] && continue
    xdotool mousemove --window "$WIN" "${point%,*}" "${point#*,}" click 1 >/dev/null 2>&1
    sleep 2
done

for key in "${KEYS[@]:-}"; do
    [[ -z "$key" ]] && continue
    xdotool key --clearmodifiers "$key" >/dev/null 2>&1
    sleep 2
done

if [[ -n "$TYPE_TEXT" ]]; then
    xdotool type --delay 40 "$TYPE_TEXT" >/dev/null 2>&1
    sleep 2
fi

if [[ "$CAPTURE_ROOT" -eq 1 ]]; then
    import -window root "$OUT" 2>/dev/null
elif ! import -window "$WIN" "$OUT" 2>/dev/null; then
    import -window root "$OUT" 2>/dev/null
fi

if [[ ! -s "$OUT" ]]; then
    echo "ERROR: capture produced no image" >&2
    exit 1
fi

echo "captured: $OUT ($(identify -format '%wx%h, %k colors' "$OUT" 2>/dev/null))"
if [[ -s /tmp/mailview.log ]]; then
    echo "--- app log (tail) ---"
    grep -vE "a11y|Gtk-WARNING|dbind|libEGL|MESA|swrast" /tmp/mailview.log | tail -15
fi
