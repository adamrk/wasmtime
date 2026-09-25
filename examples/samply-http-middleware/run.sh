#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "$SCRIPT_DIR/../.." && pwd)
OUT_DIR="$SCRIPT_DIR/out"
RUN_DIR="$OUT_DIR/run"

MIDDLEWARE_ADDR=${MIDDLEWARE_ADDR:-127.0.0.1:18080}
NGINX_ADDR=127.0.0.1:18081
DURATION=${DURATION:-15s}
THREADS=${THREADS:-4}
CONNECTIONS=${CONNECTIONS:-32}
SAMPLE_RATE=${SAMPLE_RATE:-1000}
PROFILE_OUTPUT=${PROFILE_OUTPUT:-$OUT_DIR/profile.json.gz}
WASMTIME_BUILD_PROFILE=${WASMTIME_BUILD_PROFILE:-profiling}
WASMTIME_BIN=${WASMTIME_BIN:-$REPO_ROOT/target/$WASMTIME_BUILD_PROFILE/wasmtime}

for tool in nginx wrk samply curl cargo pgrep; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "error: required command '$tool' was not found" >&2
        exit 1
    }
done

if [[ $# -gt 1 ]]; then
    echo "usage: $0 [middleware.component.wasm]" >&2
    exit 2
fi

if [[ $# -eq 1 ]]; then
    COMPONENT=$(realpath "$1")
else
    COMPONENT=$("$SCRIPT_DIR/build-middleware.sh" | tail -n 1)
fi

if [[ ${SKIP_WASMTIME_BUILD:-0} != 1 ]]; then
    cargo build \
        --manifest-path "$REPO_ROOT/Cargo.toml" \
        --profile "$WASMTIME_BUILD_PROFILE" \
        --bin wasmtime
fi

[[ -x "$WASMTIME_BIN" ]] || {
    echo "error: Wasmtime binary is not executable: $WASMTIME_BIN" >&2
    exit 1
}
[[ -f "$COMPONENT" ]] || {
    echo "error: middleware component does not exist: $COMPONENT" >&2
    exit 1
}

rm -rf -- "$RUN_DIR"
mkdir -p "$RUN_DIR/nginx/logs" "$(dirname -- "$PROFILE_OUTPUT")"

wasmtime_pid=
nginx_pid=
samply_pid=
cleanup() {
    local status=$?
    trap - EXIT INT TERM
    if [[ -n "$samply_pid" ]]; then
        kill -TERM "$samply_pid" 2>/dev/null || true
        wait "$samply_pid" 2>/dev/null || true
    fi
    if [[ -n "$wasmtime_pid" ]]; then
        kill -INT "$wasmtime_pid" 2>/dev/null || true
        wait "$wasmtime_pid" 2>/dev/null || true
    fi
    if [[ -n "$nginx_pid" ]]; then
        kill -TERM "$nginx_pid" 2>/dev/null || true
        wait "$nginx_pid" 2>/dev/null || true
    fi
    exit "$status"
}
trap cleanup EXIT INT TERM

nginx -p "$RUN_DIR/nginx" -c "$SCRIPT_DIR/nginx.conf" \
    >"$RUN_DIR/nginx.stdout.log" 2>&1 &
nginx_pid=$!

for _ in $(seq 1 100); do
    curl --silent --fail "http://$NGINX_ADDR/health" >/dev/null && break
    if ! kill -0 "$nginx_pid" 2>/dev/null; then
        echo "error: nginx exited; see $RUN_DIR/nginx.stdout.log" >&2
        exit 1
    fi
    sleep 0.05
done
curl --silent --fail "http://$NGINX_ADDR/health" >/dev/null || {
    echo "error: nginx did not become ready; see $RUN_DIR/nginx/logs/error.log" >&2
    exit 1
}

samply record \
    --save-only \
    --rate "$SAMPLE_RATE" \
    --output "$PROFILE_OUTPUT" \
    -- \
    "$WASMTIME_BIN" serve \
        --addr "$MIDDLEWARE_ADDR" \
        --profile=perfmap \
        -S cli \
        --max-concurrent-requests "$CONNECTIONS" \
        "$COMPONENT" \
    >"$RUN_DIR/profiled-command.log" 2>&1 &
samply_pid=$!

for _ in $(seq 1 200); do
    curl --silent --fail "http://$MIDDLEWARE_ADDR/health" >/dev/null && break
    if ! kill -0 "$samply_pid" 2>/dev/null; then
        echo "error: Samply or Wasmtime exited; see $RUN_DIR/profiled-command.log" >&2
        exit 1
    fi
    sleep 0.05
done
curl --silent --fail "http://$MIDDLEWARE_ADDR/health" >/dev/null || {
    echo "error: middleware did not become ready; see $RUN_DIR/profiled-command.log" >&2
    exit 1
}

wasmtime_pid=$(pgrep -P "$samply_pid" -n) || {
    echo "error: could not find the Wasmtime process started by Samply" >&2
    exit 1
}

echo "Profiling Wasmtime PID $wasmtime_pid (Samply PID $samply_pid)"
echo "Load: wrk -t$THREADS -c$CONNECTIONS -d$DURATION http://$MIDDLEWARE_ADDR/"
wrk -t"$THREADS" -c"$CONNECTIONS" -d"$DURATION" "http://$MIDDLEWARE_ADDR/"
sleep 0.5

# Stop Wasmtime after the workload so Samply can consume the remaining perf events and write a
# complete profile.
kill -INT "$wasmtime_pid"
wait "$wasmtime_pid" 2>/dev/null || true
wasmtime_pid=

if ! wait "$samply_pid"; then
    samply_pid=
    echo "error: Samply failed; see $RUN_DIR/profiled-command.log" >&2
    exit 1
fi
samply_pid=

echo "Profile written to $PROFILE_OUTPUT"
echo "Open it with: samply load $PROFILE_OUTPUT"
