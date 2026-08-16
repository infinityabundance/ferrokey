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
# moved into a `session-99.scope` cgroup before connecting; the court's own
# shell lives in ITS OWN logind session (SSH/PAM assigns a session-N.scope),
# never in session-99.scope — so the broker sees it as out-of-session.
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

# ── SESSION.AUTO.RESOLVES: `session_scope: auto` binds the broker's OWN ────
# session scope. The whole broker tree is moved into the court session cgroup
# BEFORE start, so the runtime broker's own /proc/self/cgroup carries
# session-99.scope and auto must resolve exactly that — no hard-coded number
# anywhere in the config.
AUTO_SOCK=/run/ferrokeyd/ferrokeyd-auto.sock
sudo chown root:root "$PAYLOAD/fixtures/ferrokeyd-auto.yaml" "$PAYLOAD/fixtures/ferrokeyd-auto-headless.yaml"
sudo chmod 0644 "$PAYLOAD/fixtures/ferrokeyd-auto.yaml" "$PAYLOAD/fixtures/ferrokeyd-auto-headless.yaml"
sudo env CG="$CG" PAYLOAD="$PAYLOAD" OUT="$OUT" bash -c '
    echo $$ > "$CG/cgroup.procs"
    nohup env RUST_LOG=info "$PAYLOAD/bin/ferrokeyd" start \
        --config "$PAYLOAD/fixtures/ferrokeyd-auto.yaml" \
        >"$OUT/ferrokeyd-auto.log" 2>&1 &
'
sleep 2
if [ -S "$AUTO_SOCK" ]; then
    ok "SESSION.AUTO.001 auto broker listening"
else
    bad "SESSION.AUTO.001 auto broker did not start"
    cat "$OUT/ferrokeyd-auto.log"
fi
if grep -q "auto-resolved session scope 'session-99.scope'" "$OUT/ferrokeyd-auto.log" 2>/dev/null; then
    ok "SESSION.AUTO.001 auto resolved the broker's own session scope"
else
    bad "SESSION.AUTO.001 auto resolution not observed"
    cat "$OUT/ferrokeyd-auto.log"
fi

# in-session client on the auto-bound socket is authorized
sudo env CG="$CG" AUTO_SOCK="$AUTO_SOCK" CLIENT="$PAYLOAD/courts/fk-client.py" sh -c '
    echo $$ > "$CG/cgroup.procs"
    exec setpriv --reuid=1000 --regid=1000 --clear-groups \
        python3 "$CLIENT" --socket "$AUTO_SOCK" handshake key-down 30 key-up 30 release-all
' >"$OUT/auto-in-session.log" 2>&1
if grep -q "handshake: ok" "$OUT/auto-in-session.log" 2>/dev/null \
    && grep -q "release-all: ok" "$OUT/auto-in-session.log" 2>/dev/null; then
    ok "SESSION.AUTO.002 in-session client authorized on the auto-bound socket"
else
    bad "SESSION.AUTO.002 in-session client was not served"
    cat "$OUT/auto-in-session.log"
fi

# out-of-session client is rejected at authorize (same as the explicit mode)
set +e
python3 "$PAYLOAD/courts/fk-client.py" --socket "$AUTO_SOCK" handshake key-down 30 key-up 30 release-all >"$OUT/auto-out-session.log" 2>&1
AUTO_OUT_RC=$?
set -e
if grep -q "handshake: FAILED" "$OUT/auto-out-session.log" 2>/dev/null; then
    ok "SESSION.AUTO.003 out-of-session client rejected on the auto-bound socket"
else
    bad "SESSION.AUTO.003 out-of-session client was NOT rejected (rc=$AUTO_OUT_RC)"
    cat "$OUT/auto-out-session.log"
fi

# ── SESSION.AUTO.REFUSES_HEADLESS: outside any session scope, auto must ─────
# fail startup — never silently fall back to UID/GID authorization.
#
# A real headless context has NO logind session scope in its cgroup path
# (systemd service slice, cron, sshd without pam_systemd). The court shell
# itself lives in an SSH logind session — pam_systemd assigns every login a
# session-N.scope — so the headless context is synthesized faithfully: the
# whole test tree is moved into a plain cgroup whose path has no session
# component, exactly like a service slice.
HEADLESS_CG=/sys/fs/cgroup/ferrokey-headless
sudo mkdir -p "$HEADLESS_CG"
SERVE_BEFORE=$(pgrep -cf "proc/self/exe serve" || true)
set +e
sudo env HEADLESS_CG="$HEADLESS_CG" PAYLOAD="$PAYLOAD" OUT="$OUT" bash -c '
    echo $$ > "$HEADLESS_CG/cgroup.procs"
    exec env RUST_LOG=info "$PAYLOAD/bin/ferrokeyd" start \
        --config "$PAYLOAD/fixtures/ferrokeyd-auto-headless.yaml" \
        >"$OUT/ferrokeyd-auto-headless.log" 2>&1
'
HEADLESS_RC=$?
set -e
SERVE_AFTER=$(pgrep -cf "proc/self/exe serve" || true)
if [ "$HEADLESS_RC" -ne 0 ]; then
    ok "SESSION.AUTO.004 headless auto start refused (rc=$HEADLESS_RC)"
else
    bad "SESSION.AUTO.004 headless auto start unexpectedly succeeded"
fi
if grep -q "not inside a logind session scope" "$OUT/ferrokeyd-auto-headless.log" 2>/dev/null \
    && grep -q "refusing to fall back" "$OUT/ferrokeyd-auto-headless.log" 2>/dev/null; then
    ok "SESSION.AUTO.004 refusal message logged (no silent UID/GID fallback)"
else
    bad "SESSION.AUTO.004 refusal message not logged"
    cat "$OUT/ferrokeyd-auto-headless.log"
fi
if [ "$SERVE_AFTER" -eq "$SERVE_BEFORE" ]; then
    ok "SESSION.AUTO.004 no broker process appeared ($SERVE_BEFORE -> $SERVE_AFTER)"
else
    bad "SESSION.AUTO.004 a broker process appeared ($SERVE_BEFORE -> $SERVE_AFTER)"
fi

# ── the CLI accepts `--session-scope auto`; headless it refuses ─────────────
# (mirrors serve's Auto mode at the diagnostic level; run from the same
# synthetic headless cgroup, because the court shell's own SSH login is a
# pam_systemd session).
set +e
sudo env HEADLESS_CG="$HEADLESS_CG" PAYLOAD="$PAYLOAD" OUT="$OUT" bash -c '
    echo $$ > "$HEADLESS_CG/cgroup.procs"
    exec "$PAYLOAD/bin/ferrokeyd" sandbox-probe --session-scope auto \
        >"$OUT/sandbox-probe-auto.txt" 2>&1
'
AUTO_PROBE_RC=$?
set -e
if [ "$AUTO_PROBE_RC" -ne 0 ] \
    && grep -q "not inside a logind session scope" "$OUT/sandbox-probe-auto.txt" 2>/dev/null; then
    ok "SESSION.AUTO.005 sandbox-probe --session-scope auto refuses headless"
else
    bad "SESSION.AUTO.005 sandbox-probe --session-scope auto unexpected (rc=$AUTO_PROBE_RC)"
    cat "$OUT/sandbox-probe-auto.txt" 2>/dev/null
fi

finish_court "court" "session-lifetime"
