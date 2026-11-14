#!/usr/bin/env bash
# Waits until systemd in the container "box" has booted (task M4-18f).
set -euo pipefail
state=""
for _ in $(seq 1 90); do
  state="$(docker exec box systemctl is-system-running 2>/dev/null || true)"
  case "$state" in running | degraded) break ;; esac
  sleep 1
done
echo "systemd: $state"
case "$state" in
  running) ;;
  degraded) docker exec box systemctl --failed --no-pager ;;
  *) echo "systemd did not boot" >&2; exit 1 ;;
esac
