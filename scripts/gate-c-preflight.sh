#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Gate C pre-flight: is this machine ready for a controlled run of the Rust
# backend on the panel?
#
# Read-only. It starts nothing, stops nothing and changes nothing — it prints what
# it found and exits non-zero when the run would not mean anything.
#
# Why these checks (from the audit of the #16/#17 work):
#   - More than one EdgeLine backend against one session is not a supported state:
#     KWin's scripting saturates, some feeds then never arm, and two agents on the
#     panel is the cross-process version of the two-writer hazard.
#   - The C++ agent must not run: it is today's hardware owner, opening the panel
#     per exchange and shelling out to ddcutil. With both up they contend for the
#     i2c bus and the dev.edgeline.Windows name.
#   - A stale /tmp backend from a test run can hold the HID node and a D-Bus name
#     and would make the acceptance run unreliable for reasons that are not the code
#     under test.
#
# It deliberately does NOT assert on who else holds /dev/hidraw*: Wine/Proton HID
# passthrough holds every node on a typical gaming desktop, hidraw is multi-open and
# each holder has its own report queue, so that is information, not a failure.
#
# Usage: scripts/gate-c-preflight.sh
# Exit:  0 ready for the run, 1 not ready, 2 cannot check (missing tools).

set -u

expect=""
case "${1:-}" in
    --expect)
        expect="${2:-}"
        case "$expect" in
            rust|agent) ;;
            *) printf 'usage: %s [--expect rust|agent]\n' "$0" >&2; exit 2 ;;
        esac
        ;;
    "") ;;
    -h|--help)
        printf 'usage: %s [--expect rust|agent]\n' "$0"
        printf '  --expect rust   after starting the host with EDGELINE_BACKEND=rust,\n'
        printf '                  require that the agent is gone and Rust owns the panel\n'
        exit 0
        ;;
    *) printf 'usage: %s [--expect rust|agent]\n' "$0" >&2; exit 2 ;;
esac

runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
socket="$runtime/edgeline-rust/backend.sock"
legacy_socket="$runtime/edgeline.sock"
problems=0

say() { printf '%s\n' "$*"; }
ok() { printf '  ok    %s\n' "$*"; }
bad() { printf '  FAIL  %s\n' "$*"; problems=$((problems + 1)); }
info() { printf '  ..    %s\n' "$*"; }

# Processes that belong to EdgeLine. Scanned through /proc rather than `pgrep -f`,
# because a pattern on the command line would match this script's own invocation.
#
# Both argv[0] *and* the command line are matched: Electron rewrites argv[0] to the
# whole command line, so its basename is the app directory ("ui") and a basename
# test misses the UI entirely — which is exactly the process the operator has to
# quit first. The children (renderer, gpu, zygote) keep their parent's config dir,
# so they are caught by the second pattern.
edgeline_processes() {
    for entry in /proc/[0-9]*/cmdline; do
        pid=${entry#/proc/}
        pid=${pid%/cmdline}
        [ "$pid" = "$$" ] && continue
        [ "$pid" = "$PPID" ] && continue
        argv0=$(tr '\0' '\n' < "$entry" 2>/dev/null | head -n 1)
        command=$(tr '\0' ' ' < "$entry" 2>/dev/null | cut -c1-300)
        name=""
        case "${argv0##*/}" in
            edgeline-backend|edgeline-desktop|edgeline-agent|edgeline-reserve)
                name="${argv0##*/}"
                ;;
        esac
        if [ -z "$name" ]; then
            case "$command" in
                *--class=edgeline*) name="electron (EdgeLine UI)" ;;
                */.config/edgeline*) name="electron child (EdgeLine UI)" ;;
            esac
        fi
        [ -n "$name" ] && printf '%s\t%s\n' "$pid" "$name"
    done
}

for tool in systemctl tr readlink; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        say "cannot check: '$tool' is missing"
        exit 2
    fi
done
if [ ! -d /proc/self ]; then
    say "cannot check: /proc is not available"
    exit 2
fi

say "Gate C pre-flight ($(date '+%Y-%m-%d %H:%M'))"
say ""

say "1. EdgeLine processes"
running=$(edgeline_processes)
if [ -n "$running" ] && [ "$expect" = "rust" ]; then
    # Expected here: the host and its backend are supposed to be running.
    info "running (expected after a host start):"
    printf '%s\n' "$running" | sed 's/^/          /'
elif [ -n "$running" ]; then
    bad "EdgeLine processes are already running; the run must start from a clean slate:"
    printf '%s\n' "$running" | while IFS="$(printf '\t')" read -r pid argv0; do
        printf '          %s  %s\n' "$pid" "$argv0"
    done
    info "quit EdgeLine from the tray first, then: systemctl --user stop 'app-edgeline@*.service'"
    info "(stopping the unit alone leaves the UI running, and the UI starts an agent again)"
else
    ok "none"
fi

if [ "$expect" = "rust" ]; then
    # This mode runs *after* the host started, so the host's own processes are
    # expected: only the agent and the Electron UI mean the data plane is not Rust.
    agent_or_electron=$(printf '%s\n' "$running" | grep -E 'edgeline-agent|electron' || true)
    if [ -n "$agent_or_electron" ]; then
        bad "cutover mode: the old data plane is still up:"
        printf '%s\n' "$agent_or_electron" | sed 's/^/          /'
    else
        ok "cutover mode: no agent, no Electron UI"
    fi
    if [ -S "$legacy_socket" ]; then
        bad "cutover mode: $legacy_socket still exists — the agent is the data plane"
    else
        ok "cutover mode: no agent socket"
    fi
    if printf '%s\n' "$running" | grep -q edgeline-agent; then
        bad "cutover mode: edgeline-agent is running"
    else
        ok "cutover mode: no edgeline-agent process"
    fi
fi

say ""
say "2. Autostart unit"
if systemctl --user is-active --quiet 'app-edgeline@autostart.service' 2>/dev/null; then
    bad "app-edgeline@autostart.service is active and owns the old path (C++ agent)"
    info "stop it: systemctl --user stop app-edgeline@autostart.service"
else
    ok "not active"
fi

say ""
say "3. Sockets"
if [ -S "$socket" ]; then
    bad "$socket exists: a Rust backend is already running on the production socket"
else
    ok "$socket absent"
fi
if [ -S "$legacy_socket" ]; then
    bad "$legacy_socket exists: the C++ agent still owns the panel"
    info "it disappears when the agent stops; if it lingers, the agent did not exit"
else
    ok "$legacy_socket absent"
fi

say ""
say "4. Who holds the panel's HID node (informational)"
holders=$(for fd in /proc/[0-9]*/fd/*; do
    [ "$(readlink "$fd" 2>/dev/null)" = "/dev/hidraw1" ] || continue
    pid=${fd#/proc/}
    pid=${pid%%/*}
    comm=$(cat "/proc/$pid/comm" 2>/dev/null || echo "?")
    printf '          pid %s  %s\n' "$pid" "$comm"
done)
if [ -n "$holders" ]; then
    printf '%s\n' "$holders"
    info "Wine/Proton passthrough is expected here and does not block our open"
else
    info "nobody holds /dev/hidraw1"
fi

say ""
if [ "$problems" -eq 0 ]; then
    ok "ready: start the Rust backend and drive the panel by hand"
    say ""
    say "The run to perform (in this order):"
    say "  1. cargo build --locked --release --manifest-path tauri-app/Cargo.toml"
    say "  2. EDGELINE_BACKEND=rust tauri-app/target/release/edgeline-desktop"
    say "  3. touch the panel: the ripple overlay moves and disappears, no stuck dot"
    say "  4. unplug and replug the panel: device.state and the picture values recover"
    say "  5. apply one profile by hand and one per-app rule, then check its restore"
    say "  6. rollback drill: stop it, run the Electron path, confirm the panel responds"
    exit 0
fi

say "NOT ready: fix the FAIL lines above first."
exit 1
