#!/bin/sh
# Silent, idempotent RustWTM removal. RUSTWTM_PURGE=1 also deletes the
# configuration, history and logs. Exit codes: 0 removed or not installed,
# 2 not root, 5 removal failed.
set -u
LOG=/var/log/rustwtm-deploy.log
[ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 2; }
if ! dpkg-query -W -f='${Status}' rustwtm 2>/dev/null | grep -q 'install ok installed'; then
    exit 0
fi
action=remove
[ "${RUSTWTM_PURGE:-0}" = "1" ] && action=purge
DEBIAN_FRONTEND=noninteractive apt-get "$action" -y -q rustwtm >>"$LOG" 2>&1 || exit 5
printf '%s rustwtm %sd\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$action" >>"$LOG"
exit 0
