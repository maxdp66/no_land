#!/bin/bash
# Supported release upgrade: Ubuntu 22.04 -> 24.04. Run from a terminal.
set -Eeuo pipefail
STATE=/var/lib/noland/distro-upgrade
SELF=/usr/local/lib/noland/upgrade-vm.sh
UNIT=noland-distro-upgrade.service

log() { printf '[noland-upgrade] %s\n' "$*"; }
os_codename() { (. /etc/os-release; [[ "$ID" == ubuntu ]] || exit 1; printf '%s\n' "$VERSION_CODENAME"); }
user_run() {
  runuser -u "$TARGET_USER" -- env HOME="$USER_HOME" XDG_RUNTIME_DIR="/run/user/$USER_UID" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$USER_UID/bus" "$@"
}
apt_run() {
  env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l apt-get \
    -o DPkg::Lock::Timeout=600 -o Acquire::Retries=3 \
    -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold "$@"
}
# Choose an official Noble version, even if a leftover Jammy/PPA version is higher.
# Never change a package checksum or install an unsigned third-party artifact.
noble_version() {
  local package="$1" version best=""
  while IFS= read -r version; do
    [[ -n "$version" ]] || continue
    if [[ -z "$best" ]] || dpkg --compare-versions "$version" gt "$best"; then best="$version"; fi
  done < <(apt-cache madison "$package" | awk -F '|' \
    '$3 ~ /\/\/(archive|security|ports)\.ubuntu\.com\// && $3 ~ / noble(-updates|-security)?\// {gsub(/^[ \t]+|[ \t]+$/, "", $2); print $2}')
  [[ -n "$best" ]] || { log "No Noble version available for $package; check Ubuntu sources." >&2; return 1; }
  printf '%s\n' "$best"
}
disable_stale_wine_source() {
  # WineHQ Jammy sources sometimes survive a manual upgrade. Preserve the file
  # while disabling only stale WineHQ entries; do not blindly rewrite suites.
  python3 - <<'PYTHON'
from pathlib import Path
import re
paths = [Path('/etc/apt/sources.list')]
paths += list(Path('/etc/apt/sources.list.d').glob('*.list'))
paths += list(Path('/etc/apt/sources.list.d').glob('*.sources'))
for path in paths:
    if not path.exists():
        continue
    original = path.read_text()
    if path.suffix == '.sources':
        entries = original.split('\n\n')
        for i, entry in enumerate(entries):
            if re.search(r'^URIs:.*dl\.winehq\.org', entry, re.M) and re.search(r'^Suites:.*\bjammy\b', entry, re.M):
                if re.search(r'^Enabled:', entry, re.M):
                    entry = re.sub(r'^Enabled:.*$', 'Enabled: no', entry, flags=re.M)
                else:
                    entry = entry.rstrip('\n') + '\nEnabled: no\n'
                entries[i] = entry
        updated = '\n\n'.join(entries)
    else:
        updated = ''.join('# Noland disabled stale WineHQ source: ' + line
                          if re.match(r'\s*deb(?:-src)?\s', line) and 'dl.winehq.org' in line and re.search(r'\bjammy\b', line)
                          else line for line in original.splitlines(keepends=True))
    if updated != original:
        path.write_text(updated)
        print('Disabled stale WineHQ Jammy source in', path)
PYTHON
}
repair_packages() {
  local package version
  local packages=(libspa-0.2-modules libpipewire-0.3-0t64 libpipewire-0.3-modules
    pipewire-bin pipewire pipewire-pulse libwireplumber-0.4-0 wireplumber
    libroc0.3 xdg-desktop-portal)
  for package in libspa-0.2-bluetooth libspa-0.2-jack libpipewire-0.3-common pipewire-alsa libpipewire-0.3-dev libspa-0.2-dev; do
    if dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -qx 'install ok installed'; then
      packages+=("$package")
    fi
  done
  local pinned=()
  for package in "${packages[@]}"; do
    version=$(noble_version "$package")
    pinned+=("$package=$version")
  done
  if ! dpkg-query -W -f='${Status}' sunshine 2>/dev/null | grep -qx 'install ok installed'; then
    [[ -f "$STATE/sunshine.deb" ]] || {
      log 'Sunshine is missing and no preserved package is available. Restore a Noble-compatible Sunshine package.' >&2
      return 1
    }
    pinned+=("$STATE/sunshine.deb")
  fi
  apt_run -s --fix-broken --allow-downgrades --no-remove install "${pinned[@]}"
  apt_run -y --fix-broken --allow-downgrades --no-remove install "${pinned[@]}"
  dpkg --configure -a
  apt_run check
  # Fix the audio/library dependency graph first. Combining missing KDE with
  # broken old PipeWire packages prevents APT from resolving KDE's new QML deps.
  log 'Streaming libraries repaired; restoring the KDE X11 desktop.'
  local desktop=(plasma-workspace plasma-desktop kwin-x11
    qml-module-org-kde-pipewire libkpipewire5 libkpipewiredmabuf5 libkpipewirerecord5)
  pinned=()
  for package in "${desktop[@]}"; do
    version=$(noble_version "$package")
    pinned+=("$package=$version")
  done
  apt_run -s --fix-broken --allow-downgrades --no-remove install "${pinned[@]}"
  apt_run -y --fix-broken --allow-downgrades --no-remove install "${pinned[@]}"
  apt_run check
  test -x /usr/bin/startplasma-x11
  # These are required by Noland's dedicated services, even without a display
  # manager or metapackage. Keep later autoremove from discarding the desktop.
  apt-mark manual sunshine plasma-workspace plasma-desktop kwin-x11 pipewire pipewire-pulse wireplumber
}
repair_audio() {
  loginctl enable-linger "$TARGET_USER"
  systemctl start "user@$USER_UID.service"
  # Both global and per-user masks can survive a distro upgrade.
  systemctl --global unmask pipewire.service pipewire.socket pipewire-pulse.service pipewire-pulse.socket wireplumber.service
  user_run systemctl --user unmask pipewire.service pipewire.socket pipewire-pulse.service pipewire-pulse.socket wireplumber.service
  user_run systemctl --user disable --now pulseaudio.service pulseaudio.socket || true
  user_run systemctl --user mask pulseaudio.service pulseaudio.socket
  local config="$USER_HOME/.config/pipewire/pipewire.conf.d"
  install -d -o "$TARGET_USER" -g "$(id -gn "$TARGET_USER")" "$config"
  # Retain the existing provisioning profile when present.
  if [[ ! -f "$config/70-noland-sunshine-audio.conf" ]]; then
    cat > "$config/70-noland-sunshine-audio.conf" <<'EOF'
context.objects = [
  { factory = adapter
    args = {
      factory.name = support.null-audio-sink
      node.name = sunshine_audio
      node.description = "Noland Audio"
      media.class = "Audio/Sink"
      audio.position = [ FL FR ]
      adapter.auto-port-config = { mode = dsp monitor = true position = preserve }
    }
  }
]
EOF
    chown "$TARGET_USER:$(id -gn "$TARGET_USER")" "$config/70-noland-sunshine-audio.conf"
  fi
  user_run systemctl --user daemon-reload
  user_run systemctl --user enable pipewire.socket pipewire-pulse.socket wireplumber.service
  user_run systemctl --user restart pipewire.service pipewire-pulse.service wireplumber.service
}
verify() {
  apt_run check
  nvidia-smi
  systemctl restart noland-desktop.service sunshine.service
  local attempt
  for attempt in {1..30}; do
    if systemctl is-active --quiet noland-xorg.service noland-desktop.service sunshine.service \
      && user_run systemctl --user is-active --quiet pipewire.service pipewire-pulse.service wireplumber.service \
      && user_run pactl list short sinks | awk '$2 == "sunshine_audio" {found=1} END {exit !found}' \
      && pgrep -u "$USER_UID" -x plasmashell >/dev/null \
      && user_run env DISPLAY=:0 XAUTHORITY=/etc/X11/.Xauthority-noland xrandr --current | grep -q ' connected' \
      && python3 -c 'import socket; s=socket.create_connection(("127.0.0.1",47990),2); s.close()'; then
      user_run pactl set-default-sink sunshine_audio
      log 'Desktop, display, NVIDIA, Sunshine listener and PipeWire sink are ready. Reconnect with Play to verify streaming.'
      return 0
    fi
    sleep 2
  done
  journalctl -b -u sunshine -u noland-desktop -u noland-xorg -n 100 --no-pager
  log 'Readiness failed. See this log; repair can be retried with --repair.' >&2
  return 1
}
worker() {
  exec 9>"$STATE/lock"
  flock -n 9 || { log 'Another upgrade is running.'; return 1; }
  export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l
  TARGET_USER=$(cat "$STATE/user")
  USER_UID=$(id -u "$TARGET_USER")
  USER_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
  exec > >(tee -a "$STATE/upgrade.log") 2>&1
  trap 'status=$?; log "Failed at line $LINENO (status $status). Log: $STATE/upgrade.log"; printf failed > "$STATE/phase"; exit "$status"' ERR
  local phase
  phase=$(cat "$STATE/phase")
  case "$phase" in
    upgrade)
      [[ $(os_codename) == jammy ]]
      log 'Updating Ubuntu 22.04 before the LTS release upgrade.'
      apt_run update
      apt_run -y --no-remove install update-manager-core dpkg-repack
      # Ubuntu may remove an obsolete third-party Sunshine package. Preserve
      # the installed package before upgrading, including its existing binary.
      local staging
      staging=$(mktemp -d "$STATE/repack.XXXXXX")
      (cd "$staging"; dpkg-repack sunshine)
      local archives=("$staging"/sunshine_*.deb)
      [[ ${#archives[@]} == 1 && -f "${archives[0]}" ]]
      mv "${archives[0]}" "$STATE/sunshine.deb"
      rmdir "$staging"
      apt_run -y full-upgrade
      apt_run check
      # Restrict Ubuntu's upgrader to the next LTS, never a development release.
      sed -i 's/^Prompt=.*/Prompt=lts/' /etc/update-manager/release-upgrades
      grep -qx 'Prompt=lts' /etc/update-manager/release-upgrades
      printf upgrade-ready > "$STATE/phase"
      if [[ -f /var/run/reboot-required ]]; then
        log 'Rebooting to finish base updates; release upgrade resumes after boot.'
        systemctl reboot
        return 0
      fi
      ;;&
    upgrade|upgrade-ready)
      [[ $(os_codename) == jammy ]]
      printf repair > "$STATE/phase"
      log 'Running do-release-upgrade. Remote streaming may disconnect; this service continues.'
      do-release-upgrade -f DistUpgradeViewNonInteractive
      ;;&
    upgrade|upgrade-ready|repair)
      [[ $(os_codename) == noble ]] || { log 'Repair only supports Ubuntu 24.04 (Noble).'; return 1; }
      disable_stale_wine_source
      apt_run update
      repair_packages
      repair_audio
      # Restore the dedicated Xorg service as sole display owner.
      for manager in gdm gdm3 sddm lightdm; do
        systemctl disable --now "$manager.service" 2>/dev/null || true
        systemctl mask "$manager.service" 2>/dev/null || true
      done
      cat /proc/sys/kernel/random/boot_id > "$STATE/reboot-from"
      printf verify > "$STATE/phase"
      log 'Packages repaired. Rebooting; verification resumes automatically.'
      systemctl reboot
      return 0
      ;;
    verify)
      [[ $(cat /proc/sys/kernel/random/boot_id) != "$(cat "$STATE/reboot-from")" ]] || {
        log 'Reboot still required. Reboot the VM to finish verification.'; return 1;
      }
      [[ $(os_codename) == noble ]]
      # Reconcile packages again after reboot before declaring success. This
      # also recovers jobs that reached this phase with an older helper.
      log 'Checking Noble streaming and KDE packages after reboot.'
      disable_stale_wine_source
      apt_run update
      repair_packages
      verify
      printf complete > "$STATE/phase"
      systemctl disable "$UNIT"
      ;;
    complete) log 'Upgrade already complete.' ;;
    *) log 'Previous attempt failed. Inspect upgrade.log and rerun with --repair on Noble.'; return 1 ;;
  esac
}
main() {
  case "${1:-}" in
    --help|-h)
      echo 'Usage: upgrade-noland-vm.sh [--repair] [--target-user USER]'
      echo 'Ubuntu 22.04 -> 24.04 LTS; --repair restores an already upgraded Noble VM.'
      echo 'Runs detached, repairs KDE/PipeWire, reboots as needed, then verifies services.'
      echo 'Back up games and VM data first. Logs: /var/lib/noland/distro-upgrade/upgrade.log'
      return ;;
  esac
  [[ $EUID == 0 ]] || exec sudo bash "$0" "$@"
  if [[ ${1:-} == --worker ]]; then worker; return; fi
  local mode=upgrade target="${SUDO_USER:-user}"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --repair) mode=repair; shift ;;
      --target-user) target="${2:?Missing target user}"; shift 2 ;;
      *) log "Unknown option: $1" >&2; return 2 ;;
    esac
  done
  [[ "$target" != root ]] && id "$target" >/dev/null
  local codename
  codename=$(os_codename)
  [[ "$mode:$codename" == upgrade:jammy || "$mode:$codename" == repair:noble ]] || {
    log 'Supported modes: upgrade on Ubuntu 22.04; --repair on Ubuntu 24.04.' >&2; return 1;
  }
  if systemctl is-active --quiet "$UNIT"; then log 'Upgrade is already running.'; return 1; fi
  install -d -m 0700 "$STATE" /usr/local/lib/noland
  # Backups are root-only because Sunshine configuration can contain credentials.
  local backup="$STATE/backup-$(date +%Y%m%d-%H%M%S)"
  mkdir -m 0700 "$backup"
  cp -a /etc/apt "$backup/"
  for path in /etc/sunshine /etc/systemd/system/noland-desktop.service /etc/systemd/system/noland-xorg.service /etc/systemd/system/sunshine.service; do
    [[ ! -e "$path" ]] || cp -a "$path" "$backup/"
  done
  dpkg-query -W > "$backup/packages.txt"
  if [[ "$(readlink -f "$0")" != "$SELF" ]]; then
    install -m 0755 "$(readlink -f "$0")" "$SELF"
  fi
  printf '%s\n' "$target" > "$STATE/user"
  printf '%s\n' "$mode" > "$STATE/phase"
  cat > "/etc/systemd/system/$UNIT" <<EOF
[Unit]
Description=Noland distro upgrade and streaming recovery
Wants=network-online.target
After=network-online.target
[Service]
Type=oneshot
ExecStart=/bin/bash $SELF --worker
TimeoutStartSec=infinity
[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable "$UNIT"
  systemctl start --no-block "$UNIT"
  log 'Upgrade started independently of this terminal. It will reboot the VM automatically.'
  log "Follow progress: sudo journalctl -fu $UNIT"
  log "Saved log: $STATE/upgrade.log"
}
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
