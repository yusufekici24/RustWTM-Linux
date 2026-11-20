#!/bin/sh
# Silent, idempotent RustWTM deployment for Ubuntu 24.04 fleets
# (ManageEngine Endpoint Central, Ansible, SCCM-like agents: runs as root,
# no prompts, result in the exit code and /var/log/rustwtm-deploy.log).
#
#   deploy-rustwtm.sh [PATH_TO_DEB]
#
# Without an argument the newest rustwtm_*_amd64.deb next to this script is
# used. Optional files next to the script:
#   config.toml              fleet configuration, installed to /var/lib/rustwtm
#   rustwtm_*.deb.sha256     checked before installing when present
# Environment:
#   RUSTWTM_SHORTCUT=1       Ctrl+Shift+Esc opens RustWTM for every GNOME user
#   RUSTWTM_FORCE=1          reinstall even when the same version is installed
#
# Exit codes: 0 installed or already current, 2 not root, 3 package not
# found, 4 checksum mismatch, 5 package installation failed, 6 the service
# did not come up, 7 config rejected.
set -u

LOG=/var/log/rustwtm-deploy.log
log() { printf '%s %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >>"$LOG"; }
fail() { code=$1; shift; log "ERROR($code): $*"; echo "rustwtm-deploy: $*" >&2; exit "$code"; }

[ "$(id -u)" -eq 0 ] || fail 2 "must run as root"
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

deb=${1:-}
if [ -z "$deb" ]; then
    deb=$(find "$here" -maxdepth 1 -name 'rustwtm_*_amd64.deb' -type f | sort -V | tail -n 1)
fi
if [ -z "$deb" ] || [ ! -f "$deb" ]; then fail 3 "no rustwtm_*_amd64.deb found"; fi

if [ -f "$deb.sha256" ]; then
    expected=$(cut -d' ' -f1 "$deb.sha256")
    actual=$(sha256sum "$deb" | cut -d' ' -f1)
    [ "$expected" = "$actual" ] || fail 4 "checksum mismatch for $(basename "$deb")"
fi

want=$(dpkg-deb -f "$deb" Version 2>/dev/null) || fail 3 "not a Debian package: $deb"
have=$(dpkg-query -W -f='${Status} ${Version}' rustwtm 2>/dev/null | awk '$3=="installed"{print $4}')

if [ "$have" = "$want" ] && [ "${RUSTWTM_FORCE:-0}" != "1" ]; then
    log "rustwtm $want already installed"
else
    log "installing rustwtm $want (was: ${have:-none})"
    export DEBIAN_FRONTEND=noninteractive
    # apt resolves the dependencies; dpkg is the fallback on offline hosts.
    if ! apt-get install -y -q --allow-downgrades -o Dpkg::Options::=--force-confold "$deb" >>"$LOG" 2>&1; then
        dpkg -i "$deb" >>"$LOG" 2>&1 || fail 5 "package installation failed (see $LOG)"
    fi
fi

if [ -f "$here/config.toml" ]; then
    if ! /usr/bin/wtm config check "$here/config.toml" >>"$LOG" 2>&1; then
        fail 7 "config.toml was rejected (see $LOG)"
    fi
    if ! cmp -s "$here/config.toml" /var/lib/rustwtm/config.toml 2>/dev/null; then
        install -D -m 0644 -o root -g root "$here/config.toml" /var/lib/rustwtm/config.toml
        log "fleet config.toml installed"
    fi
fi

if [ "${RUSTWTM_SHORTCUT:-0}" = "1" ] && [ -d /etc/dconf ]; then
    install -d -m 0755 /etc/dconf/db/local.d /etc/dconf/profile
    if ! cmp -s /usr/share/rustwtm/90-rustwtm-shortcut.dconf /etc/dconf/db/local.d/90-rustwtm-shortcut 2>/dev/null; then
        install -m 0644 /usr/share/rustwtm/90-rustwtm-shortcut.dconf /etc/dconf/db/local.d/90-rustwtm-shortcut
        [ -f /etc/dconf/profile/user ] || printf 'user-db:user\nsystem-db:local\n' > /etc/dconf/profile/user
        dconf update >>"$LOG" 2>&1 || true
        log "Ctrl+Shift+Esc default installed"
    fi
fi

systemctl enable rustwtm.service >>"$LOG" 2>&1 || true
systemctl is-active --quiet rustwtm.service || systemctl restart rustwtm.service >>"$LOG" 2>&1
i=0
while [ $i -lt 20 ]; do
    systemctl is-active --quiet rustwtm.service && [ -S /run/rustwtm/rustwtm.sock ] && { log "rustwtm $want running"; exit 0; }
    i=$((i + 1)); sleep 1
done
fail 6 "rustwtm.service is not running (journalctl -u rustwtm)"
