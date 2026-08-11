#!/usr/bin/env bash
# Wait for the court VM's sshd to accept connections.
# usage: wait-ssh.sh <host> <port> <user> <keyfile> <timeout-sec>
set -euo pipefail

HOST="$1"; PORT="$2"; USER="$3"; KEY="$4"; TIMEOUT="${5:-300}"

SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
     -o ConnectTimeout=3 -o LogLevel=ERROR -i "$KEY")

deadline=$(( $(date +%s) + TIMEOUT ))
until "${SSH[@]}" -p "$PORT" "$USER@$HOST" true 2>/dev/null; do
    if [ "$(date +%s)" -gt "$deadline" ]; then
        echo "SSH wait timed out after ${TIMEOUT}s"
        exit 1
    fi
    sleep 2
done
echo "VM ssh ready"
