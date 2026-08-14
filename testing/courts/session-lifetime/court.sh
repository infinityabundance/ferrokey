#!/usr/bin/env bash
# SESSION.LIFETIME.001 (§28, §99): a broker bound to a logind session scope
# serves only peers inside that session.
#
# The broker config (ferrokeyd-session.yaml) sets
# `session_scope: session-99.scope`. The binding is enforced twice:
#
#   * at the sandbox level — the runtime seccomp filter widens by exactly
#     one narrowly-gated `openat` (the peer cgroup lookup:
#     openat(proc_fd, "<pid>/cgroup", O_RDONLY|O_CLOEXEC)); the enforcement
#     probes prove the exact shape passes and every other open stays EPERM;
#   * at the authorize level — SO_PEERCRED UID/GID whitelist AND the peer's
#     cgroup session scope must equal the bound scope. A peer outside any
#     session (or in a different one) is refused before any protocol byte
#     is processed.
#
# The court manipulates real cgroups (cgroup v2): the in-session client is
# moved into a `session-99.scope` cgroup before connecting; the court's
# own shell stays outside it (under the sshd service slice).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock
SCOPE=session-99.scope
CG=/sys/fs/cgroup/$SCOPE

# ── the court session cgroup ────────────────────────────────────────────────
sudo mkdir -p "$CG"
if [ -d "$CG" ]; then
    ok "court session cgroup created ($CG)"
else
    bad "court session cgroup could not be created"
    finish_court FAIL "phase" "cgroup-create"
fi

# ── start the broker bound to the court session scope ───────────────────────
start_ferrokeyd "$PAYLOAD/fixtures/ferrokeyd-session.yaml"

# The serve log reports the post-freeze enforcement probes; the session-gated
# filter must show the base denials AND the three session-gate facts.
if grep -q "session_cgroup_read_allowed=true session_write_flags_denied=true session_fdcwd_denied=true" "$OUT/ferrokeyd.log" 2>/dev/null \
    && grep -q "openat_denied=true" "$OUT/ferrokeyd.log" 2>/dev/null; then
    ok "SESSION.001 broker installed the session-gated filter (probes passed)"
else
    bad "SESSION.001 session-gated filter not observed"
    cat "$OUT/ferrokeyd.log"
fi
if grep -q "bound to session scope 'session-99.scope'" "$OUT/ferrokeyd.log" 2>/dev/null; then
    ok "SESSION.001 broker bound to session-99.scope"
else
    bad "SESSION.001 broker did not announce the bound scope"
fi

# ── in-session client is authorized ─────────────────────────────────────────
# The client is moved into the session cgroup (as root) and then drops to
# the whitelisted uid 1000 with setpriv (NOT su/sudo — those invoke
# pam_systemd, which opens a NEW logind session and moves the process out
# of the test cgroup again); the broker's post-freeze lookup sees
# session-99.scope and authorizes the connection.
sudo env CG="$CG" SOCK="$SOCK" CLIENT="$PAYLOAD/courts/fk-client.py" sh -c '
    echo $$ > "$CG/cgroup.procs"
    exec setpriv --reuid=1000 --regid=1000 --clear-groups \
        python3 "$CLIENT" --socket "$SOCK" handshake key-down 30 key-up 30 release-all
' >"$OUT/in-session.log" 2>&1
if grep -q "handshake: ok" "$OUT/in-session.log" 2>/dev/null \
    && grep -q "release-all: ok" "$OUT/in-session.log" 2>/dev/null; then
    ok "SESSION.002 in-session client authorized (handshake + keys + release)"
else
    bad "SESSION.002 in-session client was not served"
    cat "$OUT/in-session.log"
fi

# ── out-of-session client is rejected at authorize ──────────────────────────
# The court's shell is NOT in session-99.scope (it lives under the sshd
# service slice), so the broker must refuse the connection before any
# protocol byte — the handshake cannot complete.
set +e
python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" handshake key-down 30 key-up 30 release-all >"$OUT/out-session.log" 2>&1
OUT_RC=$?
set -e
if grep -q "handshake: FAILED" "$OUT/out-session.log" 2>/dev/null; then
    ok "SESSION.003 out-of-session client rejected at authorize"
else
    bad "SESSION.003 out-of-session client was NOT rejected (rc=$OUT_RC)"
    cat "$OUT/out-session.log"
fi
if grep -q "not in the bound session scope" "$OUT/ferrokeyd.log" 2>/dev/null; then
    ok "SESSION.003 broker logged the session-scope refusal"
else
    bad "SESSION.003 refusal not logged"
fi

# ── the sandbox stays intact (§35, §60) ─────────────────────────────────────
# The standalone sandbox-probe with the same gate proves the base openat
# denials and the session gate at the raw syscall level.
PROBE_OUT=$("$PAYLOAD/bin/ferrokeyd" sandbox-probe --session-scope "$SCOPE" 2>&1) || true
echo "$PROBE_OUT" > "$OUT/sandbox-probe-session.txt"
if echo "$PROBE_OUT" | grep -q "openat_denied=true openat_event_dev_denied=true openat_privileged_dev_denied=true" \
    && echo "$PROBE_OUT" | grep -q "session_cgroup_read_allowed=true session_write_flags_denied=true session_fdcwd_denied=true"; then
    ok "SESSION.004 sandbox intact: base openat denials + the session gate"
else
    bad "SESSION.004 sandbox report unexpected: $PROBE_OUT"
fi

finish_court "court" "session-lifetime"
