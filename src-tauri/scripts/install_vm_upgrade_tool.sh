#!/usr/bin/env bash
set -euo pipefail
TARGET_USER="${1:?Target user required}"
SOURCE="${2:?Upgrade script required}"
DISPLAY_TOOL="${3:?Display tool required}"
[[ "$TARGET_USER" =~ ^[a-zA-Z_][a-zA-Z0-9_-]*$ ]]
[[ $EUID == 0 ]] || { echo 'Installer requires root' >&2; exit 1; }
USER_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
[[ -n "$USER_HOME" && "$TARGET_USER" != root ]]
TOOLS="$USER_HOME/Desktop/tools"
GROUP=$(id -gn "$TARGET_USER")
install -d -m 0755 /usr/local/lib/noland
install -m 0755 "$SOURCE" /usr/local/lib/noland/upgrade-vm.sh
install -m 0755 "$DISPLAY_TOOL" /usr/local/lib/noland/change-display-resolution.py
install -d -m 0755 -o "$TARGET_USER" -g "$GROUP" "$TOOLS"
# Keep the service executable root-owned; desktop tools only invoke that copy.
cat > "$TOOLS/upgrade-noland-vm.sh" <<'EOF'
#!/usr/bin/env bash
exec sudo /usr/local/lib/noland/upgrade-vm.sh "$@"
EOF
chmod 0755 "$TOOLS/upgrade-noland-vm.sh"
# Grant only this root-owned helper, with no arguments or --repair. No shell access.
rule=$(mktemp)
trap 'rm -f "$rule"' EXIT
printf '%s ALL=(root) NOPASSWD: /usr/local/lib/noland/upgrade-vm.sh "", /usr/local/lib/noland/upgrade-vm.sh --repair\n' "$TARGET_USER" > "$rule"
visudo -cf "$rule"
install -m 0440 "$rule" "/etc/sudoers.d/noland-vm-upgrade-${TARGET_USER}"
cat > "$TOOLS/Upgrade Ubuntu.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Upgrade Ubuntu (Noland)
Comment=Upgrade Ubuntu 22.04 to 24.04 and restore streaming
Exec=sudo /usr/local/lib/noland/upgrade-vm.sh
Terminal=true
Icon=system-software-update
Categories=System;
EOF
chmod 0755 "$TOOLS/Upgrade Ubuntu.desktop"
cat > "$TOOLS/change-display-resolution.sh" <<'EOF'
#!/usr/bin/env bash
exec python3 /usr/local/lib/noland/change-display-resolution.py "$@"
EOF
cat > "$TOOLS/Change Display Resolution.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Change Display Resolution
Comment=Choose a resolution and refresh rate with automatic rollback
Exec=python3 /usr/local/lib/noland/change-display-resolution.py
Terminal=true
Icon=preferences-desktop-display
Categories=Settings;
EOF
chmod 0755 "$TOOLS/change-display-resolution.sh" "$TOOLS/Change Display Resolution.desktop"
cat > "$TOOLS/README.txt" <<'EOF'
Noland desktop tools
====================

Display resolution
------------------
Open Change Display Resolution.desktop or run ./change-display-resolution.sh.
Choose an advertised resolution/refresh rate and confirm within 15 seconds.
Without confirmation the previous mode is restored, including if the terminal
closes. No sudo is needed. Use --list to list modes without changing anything.
Changes apply to the current session; rebooting or No Land's display controls
may override them. Reconnect Play if streaming needs to renegotiate.

VM distro upgrade
-----------------
Back up important VM/game data before upgrading. Streaming will disconnect.

Open a terminal in this folder and run:
  ./upgrade-noland-vm.sh
Or open Upgrade Ubuntu.desktop. KDE may ask you to trust the launcher.

Supported upgrade: Ubuntu 22.04 -> Ubuntu 24.04 LTS.
The job runs independently of the terminal, repairs KDE and PipeWire audio,
reboots the VM, and verifies the streaming services after boot.
It does not remove unused packages or overwrite Sunshine pairing/configuration.
Third-party release-specific repositories disabled by Ubuntu remain disabled;
review and re-enable only versions supporting the new distro (including WineHQ).

For an already upgraded Ubuntu 24.04 VM:
  ./upgrade-noland-vm.sh --repair

Progress:
  sudo journalctl -fu noland-distro-upgrade.service
Saved log and package/configuration backups:
  /var/lib/noland/distro-upgrade/
If the job fails, inspect the log. Reconnect with Play after verification.
Verification checks service/display/audio readiness; it cannot confirm client playback.
EOF
chown "$TARGET_USER:$GROUP" "$TOOLS/upgrade-noland-vm.sh" "$TOOLS/README.txt" "$TOOLS/Upgrade Ubuntu.desktop" "$TOOLS/change-display-resolution.sh" "$TOOLS/Change Display Resolution.desktop"
echo NOLAND_UPGRADE_TOOL_READY
