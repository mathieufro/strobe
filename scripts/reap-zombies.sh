#!/usr/bin/env bash
# reap-zombies.sh
#
# Best-effort reap of strobe processes stuck in macOS "UE" state
# (uninterruptible-exit zombies left behind by the pre-fix binary).
#
# Background: the macOS kernel marks a process "U" while a thread is parked
# in an uninterruptible kernel call, and "E" while proc_exit() is running.
# Neither flag yields to userspace signals — kill -9, kill -CONT, even
# task_for_pid based interruptions are no-ops. The process is genuinely
# wedged in kernel space, typically inside an EndpointSecurity exit-event
# notification or a freed-but-not-released VM region.
#
# What this script tries, in escalating order:
#   1. sudo kill -CONT + -KILL                 (cheap, almost never works)
#   2. sudo lldb attach + process kill         (uses task port; sometimes works)
#   3. sudo lldb attach + process detach       (just nudges the syscall)
#
# IMPORTANT: even if some zombies survive all three passes, the new
# daemon (post-fix) detects unresponsive sockets and recreates daemon.lock
# at a fresh inode. The zombies are inert — they hold no CPU, ~16K of RSS,
# and (after the fix) block nothing. You do NOT need to reboot to keep
# using strobe; reboot is only needed if you want a tidy `ps` output.

set -u

PIDS=$(pgrep -f "strobe (mcp|daemon)" 2>/dev/null || true)
if [[ -z "$PIDS" ]]; then
    echo "No strobe mcp/daemon processes found."
    exit 0
fi

UE_PIDS=()
for pid in $PIDS; do
    state=$(ps -o stat= -p "$pid" 2>/dev/null | tr -d ' ')
    if [[ "$state" == *U* && "$state" == *E* ]]; then
        UE_PIDS+=("$pid")
    fi
done

if [[ ${#UE_PIDS[@]} -eq 0 ]]; then
    echo "No UE-state strobe processes — nothing to reap."
    exit 0
fi

echo "Found ${#UE_PIDS[@]} UE-state strobe processes."

still_alive() {
    local out=()
    for pid in "$@"; do
        if kill -0 "$pid" 2>/dev/null; then
            out+=("$pid")
        fi
    done
    printf '%s\n' "${out[@]}"
}

echo "Pass 1: SIGCONT + SIGKILL ..."
sudo kill -CONT "${UE_PIDS[@]}" 2>/dev/null || true
sudo kill -KILL "${UE_PIDS[@]}" 2>/dev/null || true
sleep 1
mapfile -t REMAINING < <(still_alive "${UE_PIDS[@]}")

if [[ ${#REMAINING[@]} -eq 0 ]]; then
    echo "All zombies reaped."
    exit 0
fi
echo "  ${#REMAINING[@]} survivors."

if command -v lldb >/dev/null; then
    echo "Pass 2: lldb attach + process kill (uses task port to inject SIGKILL) ..."
    for pid in "${REMAINING[@]}"; do
        sudo lldb --batch \
            -o "process attach --pid $pid --waitfor false" \
            -o "process kill" \
            -o "exit" \
            >/dev/null 2>&1 &
    done
    wait
    sleep 2
    mapfile -t REMAINING < <(still_alive "${REMAINING[@]}")
    if [[ ${#REMAINING[@]} -eq 0 ]]; then
        echo "All zombies reaped after lldb process-kill pass."
        exit 0
    fi
    echo "  ${#REMAINING[@]} survivors."

    echo "Pass 3: lldb attach + detach (just to nudge the syscall) ..."
    for pid in "${REMAINING[@]}"; do
        sudo lldb --batch \
            -o "process attach --pid $pid --waitfor false" \
            -o "process detach" \
            -o "exit" \
            >/dev/null 2>&1 &
    done
    wait
    sleep 2
    mapfile -t REMAINING < <(still_alive "${REMAINING[@]}")
fi

if [[ ${#REMAINING[@]} -eq 0 ]]; then
    echo "All zombies reaped."
    exit 0
fi

echo
echo "${#REMAINING[@]} processes survived all reap passes (truly wedged in"
echo "kernel I/O — typically EndpointSecurity exit-event ack):"
for pid in "${REMAINING[@]}"; do
    ps -o pid,stat,command -p "$pid" 2>/dev/null | tail -n +2
done
echo
echo "These can ONLY be cleared by reboot. They are, however, harmless:"
echo "  * 0 CPU, ~16K RSS each"
echo "  * post-fix strobe detects unresponsive sockets and routes around"
echo "    the dead daemon.lock inode automatically — see"
echo "    src/daemon/server.rs (daemon_socket_responsive)"
echo
echo "If you have a no-reboot constraint (e.g. long-running Docker"
echo "containers), it is safe to leave them in place."
exit 0
