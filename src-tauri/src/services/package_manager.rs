use std::time::Duration;

use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult},
    services::remote_exec::RemoteExec,
};

/// Quiesce Ubuntu's periodic APT jobs and acquire the dpkg locks for provisioning.
///
/// Fresh cloud images frequently start `apt-daily-upgrade` while provisioning is
/// connecting. Stopping only `unattended-upgrades.service` is insufficient because
/// the apt timers can immediately launch a new process. This helper disables the
/// complete periodic chain, gives an active transaction a grace period, terminates
/// only known apt/dpkg holders if it remains stuck, and repairs interrupted dpkg
/// state after every lock is actually free.
pub async fn wait_for_dpkg_lock(remote: &RemoteExec, max_wait_secs: u64) -> AppResult<bool> {
    let script = format!(
        r#"#!/bin/bash
set -uo pipefail

LOCK_FILES="/var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock /var/lib/apt/lists/lock"
APT_UNITS="apt-daily.timer apt-daily-upgrade.timer apt-daily.service apt-daily-upgrade.service unattended-upgrades.service"
MAX_WAIT={max_wait_secs}
GRACE_SECONDS=30
TERM_GRACE_SECONDS=15
started=$(date +%s)
term_sent_at=0

sudo systemctl stop --no-block $APT_UNITS >/dev/null 2>&1 || true
sudo systemctl mask $APT_UNITS >/dev/null 2>&1 || true
sudo install -d -m 0755 /etc/apt/apt.conf.d
printf '%s\n' \
  'APT::Periodic::Enable "0";' \
  'APT::Periodic::Update-Package-Lists "0";' \
  'APT::Periodic::Unattended-Upgrade "0";' \
  | sudo tee /etc/apt/apt.conf.d/99noland-disable-periodic >/dev/null

lock_holders() {{
  for lock in $LOCK_FILES; do
    sudo fuser "$lock" 2>/dev/null || true
  done | tr ' ' '\n' | grep -E '^[0-9]+$' | sort -u
}}

is_package_process() {{
  pid="$1"
  cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)
  case "$cmd" in
    *apt.systemd.daily*|*unattended-upgrade*|*/apt-get*|*/apt\ *|*/dpkg*) return 0 ;;
    *) return 1 ;;
  esac
}}

while true; do
  holders=$(lock_holders)
  if [ -z "$holders" ]; then
    break
  fi

  now=$(date +%s)
  elapsed=$((now - started))
  if [ "$elapsed" -ge "$GRACE_SECONDS" ]; then
    for pid in $holders; do
      if is_package_process "$pid"; then
        if [ "$term_sent_at" -eq 0 ]; then
          echo "Stopping stuck package process PID $pid after ${{elapsed}}s grace"
          sudo kill -TERM "$pid" 2>/dev/null || true
        elif [ $((now - term_sent_at)) -ge "$TERM_GRACE_SECONDS" ]; then
          echo "Force-stopping package process PID $pid after TERM grace"
          sudo kill -KILL "$pid" 2>/dev/null || true
        fi
      else
        echo "Refusing to terminate unknown lock holder PID $pid" >&2
        exit 22
      fi
    done
    if [ "$term_sent_at" -eq 0 ]; then
      term_sent_at=$now
    fi
  fi

  if [ "$elapsed" -ge "$MAX_WAIT" ]; then
    echo "PACKAGE_LOCK_TIMEOUT holders=$holders" >&2
    for pid in $holders; do
      ps -p "$pid" -o pid=,ppid=,etime=,stat=,cmd= >&2 || true
    done
    exit 20
  fi
  sleep 2
done

if ! sudo timeout 300 dpkg --configure -a; then
  echo "DPKG_REPAIR_FAILED" >&2
  exit 21
fi

echo "NOLAND_DPKG_READY"
"#
    );

    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || {
        remote.ssh(&script, Duration::from_secs(max_wait_secs + 330))
    })
    .await
    .map_err(|error| {
        AppError::Command(format!("package-manager recovery join failure: {error}"))
    })??;

    if output.status_code == 0 && output.stdout.contains("NOLAND_DPKG_READY") {
        info!(details = %output.stdout.trim(), "package manager is ready for provisioning");
        return Ok(true);
    }

    warn!(
        status = output.status_code,
        stdout = %output.stdout.trim(),
        stderr = %output.stderr.trim(),
        "package manager did not become ready"
    );
    Ok(false)
}

/// Runtime packages every provisioning step needs, installed in one apt pass
/// up front so the later steps find them present and skip their own
/// `apt-get update` + install round trips.
const PROVISIONING_PACKAGES: &[&str] = &[
    // NVIDIA headless display + lifecycle agent window probing
    "x11-xserver-utils",
    "x11-utils",
    "alsa-utils",
    // Sunshine audio and the low-latency PipeWire profile
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "rtkit",
    // WireGuard tunnel and firewall
    "wireguard-tools",
    "iproute2",
    "ufw",
    // Microphone receiver runtime
    "pulseaudio-utils",
    "gstreamer1.0-tools",
    "gstreamer1.0-pipewire",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    // Agent installers and health checks
    "python3",
    "curl",
    "ca-certificates",
];

/// Marker the single apt pass leaves behind so later steps can skip a
/// redundant index refresh.
pub const APT_INDEX_STAMP: &str = "/var/lib/noland/apt-index-refreshed";

/// Refresh the apt index once and install every missing provisioning package
/// in a single transaction. Best effort: on failure each step still installs
/// what it needs itself, as before.
pub async fn install_provisioning_packages(remote: &RemoteExec) {
    if !wait_for_dpkg_lock(remote, 600).await.unwrap_or(false) {
        warn!("package manager not ready; provisioning steps will install their own packages");
        return;
    }

    let script = provisioning_packages_script(&remote.sudo_prefix());
    let remote = remote.clone();
    let output =
        match tokio::task::spawn_blocking(move || remote.ssh(&script, Duration::from_secs(1800)))
            .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                warn!(%error, "provisioning package install failed to run");
                return;
            }
            Err(error) => {
                warn!(%error, "provisioning package install join failure");
                return;
            }
        };

    if output.status_code == 0 {
        info!(details = %output.stdout.trim(), "provisioning packages ready");
    } else {
        warn!(
            status = output.status_code,
            stdout = %output.stdout.trim(),
            stderr = %output.stderr.trim(),
            "provisioning package install failed; steps will retry individually"
        );
    }
}

fn provisioning_packages_script(sudo: &str) -> String {
    let packages = PROVISIONING_PACKAGES.join(" ");
    format!(
        r#"set -uo pipefail
missing=""
for pkg in {packages}; do
  dpkg-query -W -f='${{Status}}' "$pkg" 2>/dev/null | grep -q "install ok installed" || missing="$missing $pkg"
done
if [ -z "$missing" ]; then
  echo "NOLAND_PACKAGES_PRESENT"
  exit 0
fi
echo "Missing packages:$missing"
apt_run() {{
  {sudo}env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l NEEDRESTART_SUSPEND=1 apt-get -o DPkg::Lock::Timeout=600 -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 "$@"
}}
updated=0
for attempt in 1 2 3; do
  if apt_run update -qq; then updated=1; break; fi
  echo "apt-get update attempt $attempt failed; retrying" >&2
  sleep 10
done
[ "$updated" = 1 ] || echo "apt-get update kept failing; installing from cached indexes" >&2
installable=""
for pkg in $missing; do
  if apt-cache show "$pkg" >/dev/null 2>&1; then
    installable="$installable $pkg"
  else
    echo "Package $pkg is not available on this distribution; skipping" >&2
  fi
done
if [ -n "$installable" ] && ! apt_run install -y $installable; then
  echo "NOLAND_PACKAGES_FAILED" >&2
  exit 1
fi
if [ "$updated" = 1 ]; then
  {sudo}install -d -m 0755 "$(dirname {stamp})" && {sudo}touch {stamp}
fi
echo "NOLAND_PACKAGES_INSTALLED"
"#,
        stamp = APT_INDEX_STAMP,
    )
}

#[cfg(test)]
mod tests {
    use super::provisioning_packages_script;

    #[test]
    fn provisioning_packages_script_is_valid_bash() {
        for sudo in ["", "sudo "] {
            let output = std::process::Command::new("bash")
                .arg("-n")
                .arg("-c")
                .arg(provisioning_packages_script(sudo))
                .output()
                .expect("bash available");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
