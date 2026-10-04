#!/bin/bash
# Install Brave as the VM's web browser (replacing Google Chrome) and make it
# the default browser for the streaming user. Idempotent; safe on reconnect.
set -euo pipefail

TARGET_USER="${1:?target user is required}"
KEYRING=/usr/share/keyrings/brave-browser-archive-keyring.gpg
SOURCE_LIST=/etc/apt/sources.list.d/brave-browser-release.list
REPO=https://brave-browser-apt-release.s3.brave.com

export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1
APT_OPTS=(-o DPkg::Lock::Timeout=600 -o Acquire::Retries=3)

if [[ "$(id -u)" != "0" ]]; then
  echo "Brave installer must run as root" >&2
  exit 1
fi
if ! id "$TARGET_USER" >/dev/null 2>&1; then
  echo "user '$TARGET_USER' does not exist" >&2
  exit 1
fi

installed() {
  dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q "install ok installed"
}

if ! installed brave-browser; then
  arch="$(dpkg --print-architecture)"
  case "$arch" in
    amd64|arm64) ;;
    *) echo "Brave has no Linux package for $arch" >&2; exit 1 ;;
  esac
  install -d -m 0755 /usr/share/keyrings
  curl -fsSL --proto '=https' --tlsv1.2 "$REPO/brave-browser-archive-keyring.gpg" -o "$KEYRING.tmp"
  install -m 0644 "$KEYRING.tmp" "$KEYRING"
  rm -f "$KEYRING.tmp"
  printf 'deb [signed-by=%s arch=%s] %s/ stable main\n' "$KEYRING" "$arch" "$REPO" > "$SOURCE_LIST"
  # Refresh only Brave's repository; the rest of the index is already current.
  apt-get "${APT_OPTS[@]}" update \
    -o Dir::Etc::sourcelist="$SOURCE_LIST" -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0
  apt-get "${APT_OPTS[@]}" install -y brave-browser xdg-utils
fi

# Brave replaces Chrome on these VMs.
if installed google-chrome-stable; then
  apt-get "${APT_OPTS[@]}" purge -y google-chrome-stable
fi
rm -f /etc/apt/sources.list.d/google-chrome.list /etc/apt/sources.list.d/google-chrome.sources

user_home="$(getent passwd "$TARGET_USER" | cut -d: -f6)"
runuser -u "$TARGET_USER" -- env HOME="$user_home" \
  xdg-settings set default-web-browser brave-browser.desktop 2>/dev/null \
  || runuser -u "$TARGET_USER" -- env HOME="$user_home" sh -c '
    for mime in x-scheme-handler/http x-scheme-handler/https text/html; do
      xdg-mime default brave-browser.desktop "$mime"
    done' \
  || echo "Could not set Brave as the default browser for $TARGET_USER" >&2

# Point any desktop launchers or dock pins that still name Chrome at Brave.
if [[ -d "$user_home" ]]; then
  find "$user_home/.local/share/applications" "$user_home/Desktop" -maxdepth 1 -type f -name '*.desktop' 2>/dev/null \
    | while IFS= read -r launcher; do
        sed -i -e 's/google-chrome\.desktop/brave-browser.desktop/g' \
          -e 's/^Exec=google-chrome\b/Exec=brave-browser/' "$launcher"
      done || true
fi

brave-browser --version 2>/dev/null || true
echo "NOLAND_BRAVE_READY"
