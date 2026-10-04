#!/usr/bin/env bash
# Shared helpers for the shell gates (vm-differential.sh, knob-matrix.sh).
# Sourced, not executed.

# `timeout` is GNU coreutils and absent on stock macOS. Both gates call it
# through `env`, which only searches PATH, so a shell function cannot stand
# in — write a small executable with GNU's contract instead: kill the child
# after N seconds and report exit 124. A flag file distinguishes "the
# watchdog fired" from "the child exited on its own"; polling the watchdog
# process instead races a command that finishes at the deadline.
#
# Takes the directory to hold the shim (already-created scratch is fine).
ensure_timeout() {
    command -v timeout >/dev/null 2>&1 && return 0
    local dir="${1:?ensure_timeout needs a directory for the timeout shim}"
    mkdir -p "$dir"
    cat > "$dir/timeout" <<'TIMEOUT_SH'
#!/usr/bin/env bash
secs="$1"; shift
flag="$(mktemp -u "${TMPDIR:-/tmp}/tyc-timeout.XXXXXX")"
"$@" &
pid=$!
( sleep "$secs"; : > "$flag"; kill -TERM "$pid" 2>/dev/null ) &
watcher=$!
status=0
wait "$pid" 2>/dev/null || status=$?
if [ -e "$flag" ]; then
    status=124
    rm -f "$flag"
fi
kill "$watcher" 2>/dev/null
wait "$watcher" 2>/dev/null
exit "$status"
TIMEOUT_SH
    chmod +x "$dir/timeout"
    PATH="$dir:$PATH"
    export PATH
}
