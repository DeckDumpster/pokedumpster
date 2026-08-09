#!/usr/bin/env bash
# Host-port allocation for the shell harnesses. Sourced, never executed.
#
# ── WHY THIS EXISTS (pd-r0ri) ───────────────────────────────────────────────
# Every container-tier gate publishes something on a host port, and this repo
# has now had the same bug found in four different files: the port was PICKED
# rather than taken from the kernel.
#
#   deploy/ci.sh                 instance name derived from a hash    (pd-8gjs)
#   tests/tenants/handles.sh     app port from a fixed band           (pd-z1xb)
#   tests/litestream/run.sh      MinIO port from a fixed band         (pd-8gjs)
#   tests/litestream/recreate.sh MinIO port from a fixed band         (pd-r0ri)
#   tests/litestream/drill.sh    MinIO port from a fixed band         (pd-r0ri)
#
# A hashed band gives a few hundred candidates, is deterministic per checkout,
# and has no retry when the bind fails. It collides with a concurrent run of the
# same gate, with a leftover container from a previous run, and with anything
# else on the box that happens to hold the number. The last one surfaced as
#
#   Error: rootlessport listen tcp 127.0.0.1:40090: bind: address already in use
#
# three sections after the code that chose 40090, looking nothing like a port
# clash. So the number is asked for, here, in one place, and every harness
# sources this file rather than growing a fifth copy of the idea.
#
# tests/lib/ports_test.sh proves the behaviour AND greps the tree for a relapse:
# a picked host port anywhere under tests/ or deploy/ fails that gate. It needs
# no container and no network, so deploy/ci.sh runs it as an early sub-second
# gate beside tests/lib/diagnostics_test.sh.
#
# Usage:
#   . "${REPO_DIR}/tests/lib/ports.sh"
#   MINIO_PORT=${MINIO_PORT:-$(free_port)}   # keep the override; a human may pin one

# A port nothing holds right now, verified by binding it.
#
# NOT `bind(("", 0))`, which is the obvious version and is subtly wrong for what
# these gates do with the answer. Port 0 returns a port from the EPHEMERAL
# range — the very range the kernel hands out to every outbound connection on
# the box. A gate holds its port for minutes, and any curl, podman pull or
# unrelated process that opens a socket in the meantime can be handed the same
# number; a listener bind against a live 4-tuple then fails EADDRINUSE. So take
# a random port from OUTSIDE that range and confirm it is free. The window
# between the confirmation and the caller's own bind is unavoidable in shell,
# but outside the ephemeral range nothing is going to wander into it.
#
# Stable for the life of the caller, which podman-assigns-it (the pattern
# tests/tenants/handles.sh uses, `-p 127.0.0.1::8080` then read it back) is not:
# tests/alarming/run.sh stops and starts its MinIO mid-run and every URL built
# before that has to still work afterwards.
free_port() {
	python3 - <<'PY'
import random, socket, sys

# The kernel's own answer to "which ports do I hand out unasked".
try:
    with open("/proc/sys/net/ipv4/ip_local_port_range") as f:
        eph_lo, eph_hi = (int(x) for x in f.read().split()[:2])
except OSError:
    eph_lo, eph_hi = 32768, 60999

# Above the ephemeral range by preference (61000-65535 on a default box), below
# it if some box has been configured to hand out everything up to 65535, and
# only then the whole unprivileged range — a narrow band beats no port at all.
bands = [b for b in ((eph_hi + 1, 65535), (10000, eph_lo - 1)) if b[1] - b[0] >= 64]
bands.append((1024, 65535))

for lo, hi in bands:
    for _ in range(200):
        port = random.randint(lo, hi)
        s = socket.socket()
        try:
            # No SO_REUSEADDR: a port this bind cannot have is a port the
            # caller must not be handed either.
            s.bind(("", port))
        except OSError:
            continue
        finally:
            s.close()
        print(port)
        sys.exit(0)

sys.exit("free_port: no free port found in %r" % (bands,))
PY
}
