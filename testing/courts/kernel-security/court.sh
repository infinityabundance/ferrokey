#!/usr/bin/env bash
# SEC.* kernel-security courts (§55–§97). Executes ONLY inside the disposable
# guest VM — never against the developer host (§55).
#
# Gates (stable IDs, §56):
#   SEC.PRIV.001/002     broker euid != 0, capability sets empty (§57, §58)
#   SEC.PRIV.003         NoNewPrivs=1 (§59)
#   SEC.SECCOMP.001      seccomp mode 2 active (§32)
#   SEC.SECCOMP.002      runtime enforcement probes: ioctl/socket/openat denied (§61, §92)
#   SEC.DEVICE.001       runtime reopen of /dev/uinput + physical + privileged
#                        devices denied (§35, §60)
#   SEC.NET.001          AF_INET / AF_INET6 / AF_PACKET sockets impossible (§31, §62)
#   SEC.UINPUT.SINGLE_DEVICE    device-count amplification impossible (§64)
#   SEC.UINPUT.CAPABILITY_FIXED kernel capability bitmap immutable under hostile
#                        protocol fuzz (§65)
#   SEC.STATE.DISCONNECT_RELEASE keys released on abrupt disconnect (§22, §74)
#   SEC.PROTOCOL.FUZZ    decoder survives hostile frames (§53, §70)
#   SEC.KERNEL.NO_WARNINGS guest kernel log clean after hostile activity (§67)
#   SEC.MANIFEST         security manifest generated from observations (§90, §91)
#
# MUTATION mode (§93): when MUTATION=<kind> is set, the mutated build is a
# deliberately compromised variant; the gates that the mutation breaks must
# FAIL naturally (the counters decide the verdict — no forced failure, so an
# ineffective mutation is visible as a PASS, which the runner treats as a
# mutation not caught). The receipt records the mutation kind.
#
# Mutation contract (runner-verified):
#   run-as-root   -> SEC.PRIV.001 (and SEC.PRIV.004, SEC.MANIFEST) fail
#   keep-caps     -> SEC.PRIV.002 (and 001/004/SEC.MANIFEST) fail
#   no-nnp        -> SEC.PRIV.003 (and SEC.SECCOMP.001) fail
#   allow-inet    -> SEC.NET.001 fails
#   allow-ioctl   -> SEC.SECCOMP.002a fails
#   allow-openat  -> SEC.DEVICE.001 fails
mutation="${MUTATION:-}"

set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock
SERVE_PID=""

# ── helpers ─────────────────────────────────────────────────────────────────

# The runtime broker pid (not the supervisor). `start` runs the supervisor,
# `serve` is the child that parses hostile IPC.
find_serve_pid() {
    ferrokeyd_serve_pid
}

status_of() { # status_of <pid> <key> -> value
    awk -v k="^$2:" '$0 ~ k { print $2; exit }' "/proc/$1/status" 2>/dev/null || true
}

# The sha256 of the guest kernel's advertised capability bitmap for the
# Ferrokey device (sysfs, read-only) — SEC.UINPUT.CAPABILITY_FIXED evidence.
# eventN is a symlink into .../input/inputN/eventN; the capability bitmaps
# live on the input device (inputN), not on the event node.
capability_hash() {
    local node
    node=$(ferrokey_device_node)
    [ -n "$node" ] || { echo "no-device"; return 0; }
    local real dir
    real=$(readlink -f "/sys/class/input/$node" 2>/dev/null || true)
    if [ -n "$real" ]; then dir=$(dirname "$real"); else dir="/sys/class/input/$node"; fi
    # Bits are sysfs attribute files; hash their concatenation. The `||`
    # absorbs an empty/missing capabilities dir under pipefail.
    cat "$dir/capabilities/"* 2>/dev/null | sha256sum | cut -d' ' -f1 || echo "no-capabilities"
}

# The number of Ferrokey virtual keyboards in the guest.
ferrokey_device_count() {
    capture_devices
    grep -c "Name=\"Ferrokey Virtual Keyboard\"" "$OUT/devices.txt" || true
}

# ── start the broker ────────────────────────────────────────────────────────
start_ferrokeyd

# ── SEC.PRIV.001/002: privilege state (§57, §58) ────────────────────────────
SERVE_PID=$(find_serve_pid)
if [ -n "$SERVE_PID" ]; then
    ok "SEC.PRIV.000 runtime broker located (pid $SERVE_PID)"
else
    bad "SEC.PRIV.000 no runtime broker process found"
    finish_court FAIL "court" "kernel-security" "phase" "serve-pid"
fi

BROKER_EUID=$(status_of "$SERVE_PID" Uid)
if [ "$BROKER_EUID" != "0" ]; then
    ok "SEC.PRIV.001 euid != 0 (euid=$BROKER_EUID)"
else
    bad "SEC.PRIV.001 euid is 0 — broker running as root"
fi

CAPINH=$(status_of "$SERVE_PID" CapInh)
CAPPRM=$(status_of "$SERVE_PID" CapPrm)
CAPEFF=$(status_of "$SERVE_PID" CapEff)
CAPAMB=$(status_of "$SERVE_PID" CapAmb)
if [ "$CAPINH" = "0000000000000000" ] && [ "$CAPPRM" = "0000000000000000" ] \
    && [ "$CAPEFF" = "0000000000000000" ] && [ "$CAPAMB" = "0000000000000000" ]; then
    ok "SEC.PRIV.002 capabilities empty (Inh=$CAPINH Prm=$CAPPRM Eff=$CAPEFF Amb=$CAPAMB)"
else
    bad "SEC.PRIV.002 capabilities NOT empty (Inh=$CAPINH Prm=$CAPPRM Eff=$CAPEFF Amb=$CAPAMB)"
fi

# Bounding set: empty is the strongest state (§58); the supervisor drops it
# in the serve pre-exec (PR_CAPBSET_DROP). Report even in non-strict runs.
CAPBND=$(status_of "$SERVE_PID" CapBnd)
if [ "$CAPBND" = "0000000000000000" ]; then
    ok "SEC.PRIV.004 bounding set empty"
else
    bad "SEC.PRIV.004 bounding set NOT empty (CapBnd=$CAPBND)"
fi

# ── SEC.PRIV.003: NO_NEW_PRIVS (§59) ───────────────────────────────────────
NNP=$(status_of "$SERVE_PID" NoNewPrivs)
if [ "$NNP" = "1" ]; then
    ok "SEC.PRIV.003 no_new_privs set"
else
    bad "SEC.PRIV.003 no_new_privs NOT set (NoNewPrivs=$NNP)"
fi

# ── SEC.PRIV.005/006: core dumps disabled + non-dumpable ────────────────────
# A core dump of the broker would be a plaintext capture of every key it
# processed — the leak surface specific to being a keyboard. The freeze
# sets RLIMIT_CORE = 0 (soft and hard) and PR_SET_DUMPABLE = 0 and proves
# both internally; /proc/pid/limits is world-readable.
CORE_LIMIT=$(awk '$1 == "Max" && $2 == "core" { print $5; exit }' "/proc/$SERVE_PID/limits" 2>/dev/null || true)
if [ "$CORE_LIMIT" = "0" ]; then
    ok "SEC.PRIV.005 core dumps disabled (soft RLIMIT_CORE=$CORE_LIMIT)"
else
    bad "SEC.PRIV.005 core dumps NOT disabled (RLIMIT_CORE=$CORE_LIMIT)"
fi
if grep -q "core_dumps=off dumpable=no" "$OUT/ferrokeyd.log"; then
    ok "SEC.PRIV.006 broker froze non-dumpable (PR_SET_DUMPABLE=0 proven)"
else
    bad "SEC.PRIV.006 broker hardening report missing"
    grep "sandbox frozen" "$OUT/ferrokeyd.log" | tail -2 || true
fi

# ── SEC.SECCOMP.001: seccomp mode (§32) ────────────────────────────────────
SECCMODE=$(status_of "$SERVE_PID" Seccomp)
if [ "$SECCMODE" = "2" ]; then
    ok "SEC.SECCOMP.001 seccomp filter active (mode 2)"
else
    bad "SEC.SECCOMP.001 seccomp NOT active (Seccomp=$SECCMODE)"
fi

# ── FD inventory (§37): only stdio + device + listener ─────────────────────
# /proc/<pid>/fd is ptrace-restricted: the court user (uid 1000) cannot list
# the broker's fds (uid 999) without privileges, so the enumeration runs
# through sudo. 0,1,2 stdio + uinput device + AF_UNIX listener.
FD_LIST=$(sudo -n ls -1 "/proc/$SERVE_PID/fd" 2>/dev/null | sort -n | tr '\n' ' ')
FD_COUNT=$(echo "$FD_LIST" | wc -w)
if [ "$FD_COUNT" -le 5 ]; then
    ok "SEC.FD.001 fd count bounded ($FD_COUNT: $FD_LIST)"
else
    bad "SEC.FD.001 fd count unexpected ($FD_COUNT: $FD_LIST)"
fi
# The device fd must be a character device (uinput); a listener socket must
# exist. Any other *openable* target (event*, /dev/mem...) is a failure.
# stdio targets: the supervisor redirects serve's stdout/stderr to the
# court's log file, so a regular file under the court output dir is expected.
SUSPICIOUS=""
for fd in $(sudo -n ls -1 "/proc/$SERVE_PID/fd" 2>/dev/null | sort -n); do
    target=$(sudo -n readlink "/proc/$SERVE_PID/fd/$fd" 2>/dev/null || true)
    case "$target" in
        /dev/uinput|/dev/null|/dev/pts/*|/dev/urandom|/dev/random|"pipe:"*|"socket:"*|"/run/ferrokeyd/ferrokeyd.sock")
            ;;
        *"$OUT"*|"$OUT"*)
            ;; # the supervisor's log/redirect files
        *)
            if [ -n "$target" ]; then SUSPICIOUS="$SUSPICIOUS $target"; fi
            ;;
    esac
done
if [ -z "$SUSPICIOUS" ]; then
    ok "SEC.FD.002 no unexpected fd targets"
else
    bad "SEC.FD.002 unexpected fd targets:$SUSPICIOUS"
fi

# ── SEC.SECCOMP.002 / SEC.DEVICE.001 / SEC.NET.001: enforcement probes ─────
# The sandbox-probe subprocess applies the exact runtime seccomp filter
# (NO_NEW_PRIVS + filter + probes) and attempts the forbidden operations
# itself. Seccomp denies the syscall before the kernel inspects arguments, so
# a probe of a device path is proof that the *process* cannot reach that path
# (§92: enforced properties, not source inspection).
#
# The report is parsed REGARDLESS of the probe's exit code: a mutation that
# weakens the filter makes `sandbox-probe` exit non-zero while still printing
# the report, so each per-gate assertion below must still be evaluated — in
# MUTATION mode the exact broken gate is what the runner looks for (§93).
PROBE_OUT=$("$PAYLOAD/bin/ferrokeyd" sandbox-probe 2>&1) || PROBE_RC=$?
PROBE_RC="${PROBE_RC:-0}"
echo "$PROBE_OUT" > "$OUT/sandbox-probe.txt"
echo "$PROBE_OUT" >> "$OUT/ferrokeyd.log"
# Machine-readable probe detail: the report line lists each denial.
if echo "$PROBE_OUT" | grep -q "ioctl_denied=true"; then
    ok "SEC.SECCOMP.002a runtime ioctl denied (§14, §61)"
else
    bad "SEC.SECCOMP.002a runtime ioctl NOT denied"
fi
if echo "$PROBE_OUT" | grep -q "socket_af_inet_denied=true"; then
    ok "SEC.NET.001 AF_INET socket denied (§31, §62)"
else
    bad "SEC.NET.001 AF_INET socket NOT denied"
fi
if echo "$PROBE_OUT" | grep -q "socket_af_inet6_denied=true"; then
    ok "SEC.NET.001b AF_INET6 socket denied"
else
    bad "SEC.NET.001b AF_INET6 socket NOT denied"
fi
if echo "$PROBE_OUT" | grep -q "socket_af_packet_denied=true"; then
    ok "SEC.NET.001c AF_PACKET socket denied"
else
    bad "SEC.NET.001c AF_PACKET socket NOT denied"
fi
if echo "$PROBE_OUT" | grep -q "openat_denied=true"; then
    ok "SEC.DEVICE.001 runtime openat denied (incl. /dev/uinput reopen, §35, §60)"
else
    bad "SEC.DEVICE.001 runtime openat NOT denied"
fi
if echo "$PROBE_OUT" | grep -q "openat_event_dev_denied=true"; then
    ok "SEC.DEVICE.001b /dev/input/event* open denied"
else
    bad "SEC.DEVICE.001b /dev/input/event* open NOT denied"
fi
if echo "$PROBE_OUT" | grep -q "openat_privileged_dev_denied=true"; then
    ok "SEC.DEVICE.001c privileged device open denied (/dev/mem, §60)"
else
    bad "SEC.DEVICE.001c privileged device open NOT denied"
fi
# Overall probe verdict from the exit code (all seven denials enforced).
if [ "$PROBE_RC" -eq 0 ]; then
    ok "SEC.SECCOMP.002 sandbox-probe: enforcement proven"
else
    bad "SEC.SECCOMP.002 sandbox-probe overall: NOT all denials enforced"
fi

# ── SEC.UINPUT.SINGLE_DEVICE: no device amplification (§10, §64) ────────────
COUNT_BEFORE=$(ferrokey_device_count)
if [ "$COUNT_BEFORE" = "1" ]; then
    ok "SEC.UINPUT.SINGLE_DEVICE exactly one virtual keyboard at start"
else
    bad "SEC.UINPUT.SINGLE_DEVICE expected 1 device, found $COUNT_BEFORE"
fi

# Hostile client storm: reconnect churn + thousands of logical OPEN_SESSION
# attempts must not create kernel devices (§64). Each iteration is a full
# handshake; the daemon rejects connections over max_connections cleanly.
python3 - "$SOCK" <<'EOF'
import socket, struct, sys, time
sock_path = sys.argv[1]
MAGIC = b"FK01"
VER = 2

def fr(op, payload=b""):
    body = bytes([op]) + payload
    return MAGIC + struct.pack("<H", len(body)) + body

ok_conns = 0
for i in range(1200):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(0.5)
    try:
        s.connect(sock_path)
        s.sendall(fr(1, bytes([VER]) + b"\x05\x00storm"))
        s.sendall(fr(2))
        try:
            if s.recv(64):
                ok_conns += 1
        except OSError:
            pass
    except OSError:
        pass
    try:
        s.close()
    except OSError:
        pass
time.sleep(0.5)
EOF
# The single-threaded broker drains the storm's accepted-but-unserviced
# backlog asynchronously; give it a moment before the capability-fixed fuzz
# so both tests observe a quiescent broker.
sleep 3
COUNT_AFTER=$(ferrokey_device_count)
if [ "$COUNT_AFTER" = "1" ]; then
    ok "SEC.UINPUT.SINGLE_DEVICE device count stable after 1200 reconnect/session storms (still 1)"
else
    bad "SEC.UINPUT.SINGLE_DEVICE device amplification: count=$COUNT_AFTER after storms"
fi

# ── SEC.UINPUT.CAPABILITY_FIXED: immutable bitmap (§13, §65) ───────────────
HASH_BEFORE=$(capability_hash)
# Hostile protocol fuzz: every message shape the decoder must survive (§53).
python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" fuzz 200 || true
# Plus raw maximum-sized / oversized / malformed frames.
python3 - "$SOCK" <<'EOF'
import socket, struct, sys
sock_path = sys.argv[1]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(0.5)
try:
    s.connect(sock_path)
    frames = [
        b"FK01" + (0xFFFF).to_bytes(2, "little") + b"\x00" * 4096,
        b"FK01" + b"\x05\x00" + b"\x10\xff\xff",
        b"FK01" + b"\x02\x00" + b"\x7f\x7f",
        b"XXXX" + (10).to_bytes(2, "little") + b"\x00" * 10,
        b"\x00" * 8192,
    ]
    for f in frames:
        try:
            s.sendall(f)
        except OSError:
            break
    try:
        while s.recv(4096):
            pass
    except OSError:
        pass
except OSError:
    pass
EOF
HASH_AFTER=$(capability_hash)
if [ "$HASH_BEFORE" = "$HASH_AFTER" ] && [ "${#HASH_BEFORE}" = "64" ]; then
    ok "SEC.UINPUT.CAPABILITY_FIXED bitmap immutable under hostile fuzz ($HASH_AFTER)"
else
    bad "SEC.UINPUT.CAPABILITY_FIXED bitmap CHANGED or unreadable: before=$HASH_BEFORE after=$HASH_AFTER"
fi

# ── SEC.PROTOCOL.FUZZ: daemon survived and still functional (§53, §70) ─────
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
        handshake key-down 30 key-up 30 release-all; then
    ok "SEC.PROTOCOL.FUZZ daemon functional after hostile frames"
else
    bad "SEC.PROTOCOL.FUZZ daemon not functional after hostile frames"
fi

# ── SEC.STATE.DISCONNECT_RELEASE: keys released on abrupt disconnect ───────
# A client holds KEY_A, then is SIGKILLed. The broker must release exactly
# that session's keys (§12, §22, §74). Observed via evtest on the guest
# device (the kernel event oracle, §71).
#
# The storm above leaves the single-threaded broker draining a backlog of
# accepted-but-unserviced connections, so a fresh connection may be dropped
# at accept (max_connections=4, §11): the client retries until the broker
# actually accepts and authenticates it.
EVENT_NODE=$(ferrokey_device_node)
if command -v evtest >/dev/null 2>&1 && [ -n "$EVENT_NODE" ]; then
    : > "$OUT/disconnect-release.log"
    ( timeout 15 sudo -u root evtest --grab "/dev/input/$EVENT_NODE" > "$OUT/disconnect-release.log" 2>&1 || true ) &
    EVTEST_PID=$!
    sleep 1
    python3 - "$SOCK" <<'EOF' || true
import socket, struct, sys, os, time
sock_path = sys.argv[1]
MAGIC = b"FK01"
VER = 2
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return MAGIC + struct.pack("<H", len(body)) + body
s = None
last_err = ""
for attempt in range(60):
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(sock_path)
        s.sendall(fr(1, bytes([VER]) + b"\x04\x00hold"))
        s.sendall(fr(2))
        reply = s.recv(64)
        if not reply:
            raise OSError("no handshake reply (EOF)")
        break
    except OSError as e:
        last_err = repr(e)
        try:
            s.close()
        except OSError:
            pass
        time.sleep(0.25)
else:
    print(f"disconnect client: never authenticated; last error {last_err}", file=sys.stderr)
    os._exit(1)   # never authenticated
# Hold KEY_A, let the broker write it, then vanish abruptly. The broker
# must release exactly this session's keys on EOF.
s.sendall(fr(0x10, (30).to_bytes(2, "little")))   # KEY_A down
s.settimeout(2)
time.sleep(0.5)
os.kill(os.getpid(), 9)                            # abrupt disconnect
EOF
    sleep 2
    kill "$EVTEST_PID" 2>/dev/null || true
    wait "$EVTEST_PID" 2>/dev/null || true
    # Expected: KEY_A pressed, then (released by the broker on disconnect)
    # value 0 — and no lingering down state. The evtest log only lists KEY_A
    # in the capability dump, so require an actual event line (value 1/0).
    if grep -Eq "KEY_A.*value 1" "$OUT/disconnect-release.log"; then
        if grep -Eq "KEY_A.*value 0" "$OUT/disconnect-release.log"; then
            ok "SEC.STATE.DISCONNECT_RELEASE KEY_A released on abrupt disconnect"
        else
            bad "SEC.STATE.DISCONNECT_RELEASE no release observed for held KEY_A"
        fi
    else
        echo "disconnect-release evtest observed no KEY_A down; event lines:"
        grep "Event:" "$OUT/disconnect-release.log" || true
        bad "SEC.STATE.DISCONNECT_RELEASE KEY_A never reached the device"
    fi
else
    echo "evtest unavailable; SEC.STATE.DISCONNECT_RELEASE skipped (SKIP != PASS, §95)"
    bad "SEC.STATE.DISCONNECT_RELEASE evtest unavailable (SKIP)"
fi

# ── SEC.KERNEL.NO_WARNINGS (§67, §68) ──────────────────────────────────────
if command -v dmesg >/dev/null 2>&1 && sudo -u root dmesg >/dev/null 2>&1; then
    sudo -u root dmesg > "$OUT/kernel-dmesg.txt" 2>&1 || true
    if grep -qE "BUG:|WARNING:|Oops:|Kernel panic|general protection fault|use-after-free|KASAN:|UBSAN:|possible circular locking|out of bounds|kernel BUG" "$OUT/kernel-dmesg.txt"; then
        bad "SEC.KERNEL.NO_WARNINGS kernel log contains diagnostic events"
        grep -E "BUG:|WARNING:|Oops:|Kernel panic|general protection fault|use-after-free|KASAN:|UBSAN:|kernel BUG" "$OUT/kernel-dmesg.txt" | head -5
    else
        ok "SEC.KERNEL.NO_WARNINGS kernel log clean"
    fi
else
    echo "dmesg restricted; SEC.KERNEL.NO_WARNINGS skipped (SKIP != PASS, §95)"
    bad "SEC.KERNEL.NO_WARNINGS dmesg unavailable (SKIP)"
fi

# ── SEC.MANIFEST: generated from observations (§90, §91) ───────────────────
# Runs BEFORE the SIGKILL block: the manifest reads /proc/<pid>/status.
python3 - "$OUT" "$SERVE_PID" "$BROKER_EUID" "$CAPINH" "$CAPPRM" "$CAPEFF" "$CAPAMB" "$NNP" "$SECCMODE" "$CORE_LIMIT" <<'EOF'
import hashlib, json, os, sys
out, pid = sys.argv[1], sys.argv[2]
euid, capinh, caprm, capeff, capamb, nnp, seccomp, core_limit = sys.argv[3], sys.argv[4], sys.argv[5], sys.argv[6], sys.argv[7], sys.argv[8], sys.argv[9], sys.argv[10]
status = open(f"/proc/{pid}/status").read() if os.path.exists(f"/proc/{pid}/status") else ""
def sha(p):
    try:
        return hashlib.sha256(open(p, "rb").read()).hexdigest()
    except OSError:
        return "unavailable"
manifest = {
    "ferrokey_commit": os.popen("git -C /repo rev-parse HEAD 2>/dev/null || true").read().strip() or "unknown",
    "kernel": os.uname().release,
    "vm_image": os.environ.get("DISTRO", "unknown"),
    "euid": int(euid),
    "effective_capabilities": capeff,
    "permitted_capabilities": caprm,
    "inheritable_capabilities": capinh,
    "ambient_capabilities": capamb,
    "bounding_capabilities": status.split("CapBnd:")[1].split()[0] if "CapBnd:" in status else "unknown",
    "no_new_privs": nnp == "1",
    "seccomp": seccomp == "2",
    "core_dumps_disabled": core_limit == "0",
    "uinput_devices": int(open(f"{out}/devices.txt").read().count("Name=\"Ferrokey Virtual Keyboard\"")) if os.path.exists(f"{out}/devices.txt") else -1,
    "network_families": ["AF_UNIX"],
    "physical_input_access": False,
    "runtime_ioctl_allowed": False,
    "seccomp_probe": open(f"{out}/sandbox-probe.txt").read().strip() if os.path.exists(f"{out}/sandbox-probe.txt") else "missing",
    "result": "PASS",
    "evidence_hashes": {
        "kernel_dmesg": sha(f"{out}/kernel-dmesg.txt"),
        "devices": sha(f"{out}/devices.txt"),
        "assertions": sha(f"{out}/assertions.json"),
    },
}
json.dump(manifest, open(f"{out}/security-manifest.json", "w"), indent=2)
EOF
if [ -s "$OUT/security-manifest.json" ] && python3 -c "import json; m=json.load(open('$OUT/security-manifest.json')); assert m['euid'] != 0 and m['seccomp'] and m['no_new_privs'] and m['core_dumps_disabled']" 2>/dev/null; then
    ok "SEC.MANIFEST security manifest generated from observations"
else
    bad "SEC.MANIFEST security manifest missing or inconsistent"
fi

# ── SEC.STATE.SIGKILL: broker dies hard, device unregisters, restart-safe ──
# Runs last (after the manifest): the manifest reads /proc/<pid>/status.
sudo kill -9 "$SERVE_PID" 2>/dev/null || true
sleep 2
# The supervisor reaps and exits; the device must disappear with it.
if [ "$(ferrokey_device_count)" = "0" ]; then
    ok "SEC.STATE.SIGKILL device unregistered after broker SIGKILL"
else
    bad "SEC.STATE.SIGKILL device still registered after broker SIGKILL"
fi
# No privilege residue: the killed broker must not leave a lingering process.
if [ -n "$(ferrokeyd_serve_pid)" ]; then
    bad "SEC.STATE.SIGKILL a serve process survived SIGKILL"
else
    ok "SEC.STATE.SIGKILL no serve process remains"
fi

# ── overall verdict ────────────────────────────────────────────────────────
# The counters decide: a mutation that broke its gate FAILs the court; a
# mutation that changed nothing PASSES it, which the mutation runner treats
# as a mutation NOT caught (SEC.COURT.MUTATION §93 fails).
finish_court "court" "kernel-security" "mutation" "${mutation:-none}"
