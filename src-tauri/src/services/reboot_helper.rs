use std::time::Duration;

use tokio::time::{sleep, timeout};
use tracing::{info, warn};

use crate::utils::shell;
use crate::{
    errors::{AppError, AppResult},
    services::{remote_exec::RemoteExec, sunshine::DISABLE_SCREEN_LOCK_SH},
};

/// Upper bound for the whole restart so the UI never waits indefinitely.
const RESTART_BUDGET: Duration = Duration::from_secs(5 * 60);

pub struct RebootHelperService;

impl RebootHelperService {
    /// Restarts the streaming stack (Xorg, desktop, audio, display mode and
    /// Sunshine) without rebooting the VM, so SSH and the endpoint stay put.
    pub async fn restart_services(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        info!(
            event = "instance_services_restart_start",
            target_user = target_user,
            "Service restart started"
        );

        let result = timeout(
            RESTART_BUDGET,
            Self::restart_services_inner(remote, target_user),
        )
        .await
        .unwrap_or_else(|_| {
            Err(AppError::Timeout(format!(
                "Restarting instance services did not finish within {} seconds",
                RESTART_BUDGET.as_secs()
            )))
        });

        match &result {
            Ok(_) => info!(
                event = "instance_services_restart_success",
                target_user = target_user,
                "Service restart completed successfully"
            ),
            Err(error) => warn!(
                event = "instance_services_restart_failure",
                target_user = target_user,
                error = %error,
                "Service restart failed"
            ),
        }
        result
    }

    async fn restart_services_inner(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        Self::preflight(remote, target_user).await?;
        Self::stop_streaming_stack(remote, target_user).await?;
        Self::ensure_noland_xorg(remote).await?;
        Self::start_desktop_session(remote).await?;
        Self::ensure_display_mode(remote).await?;
        let display_xauthority = Self::wait_for_user_display_ready(remote, target_user).await?;
        Self::ensure_audio_ready(remote, target_user).await?;
        Self::recover_sunshine(remote, target_user, &display_xauthority).await?;
        Ok("Instance services restarted and back online".to_string())
    }

    /// Stops Sunshine and the desktop, restarts the user audio services, and
    /// restarts Xorg. Each stop is bounded and falls back to SIGKILL so a hung
    /// unit cannot stall the restart.
    async fn stop_streaming_stack(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let script = format!(
            r#"set -uo pipefail
TARGET_USER={target_user}
TARGET_UID=$(id -u "$TARGET_USER")
stop_unit() {{
    systemctl cat "$1" >/dev/null 2>&1 || return 0
    if ! timeout 20 systemctl stop "$1"; then
        systemctl kill --signal=KILL "$1" 2>/dev/null || true
        systemctl reset-failed "$1" 2>/dev/null || true
    fi
}}
stop_unit sunshine.service
stop_unit noland-desktop.service
sudo -u "$TARGET_USER" env XDG_RUNTIME_DIR="/run/user/$TARGET_UID" \
    timeout 20 systemctl --user restart pipewire.service pipewire-pulse.service wireplumber.service 2>/dev/null || true
if [ "$(systemctl show noland-xorg.service --property=LoadState --value 2>/dev/null)" = "loaded" ]; then
    stop_unit noland-xorg.service
    pkill -9 -x Xorg 2>/dev/null || true
fi
echo STREAMING_STACK_STOPPED"#,
            target_user = shell::quote(target_user),
        );
        let command = format!("sudo bash -lc {}", shell::quote(&script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(90)).await?;
        if !output.stdout.contains("STREAMING_STACK_STOPPED") {
            return Err(AppError::Provisioning(format!(
                "Could not stop instance services for restart. stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }
        info!("Streaming services stopped for restart");
        Ok(())
    }

    async fn start_desktop_session(remote: &RemoteExec) -> AppResult<()> {
        let script = r#"set -euo pipefail
if ! systemctl cat noland-desktop.service >/dev/null 2>&1; then
    echo NOLAND_DESKTOP_NOT_INSTALLED
    exit 0
fi
systemctl start noland-desktop.service
echo NOLAND_DESKTOP_STARTED"#;
        let command = format!("sudo bash -lc {}", shell::quote(script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(45)).await?;
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "The desktop session could not be started. stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }
        info!(result = %output.stdout.trim(), "Desktop session start requested");
        Ok(())
    }

    async fn preflight(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
if ! id "$TARGET_USER" >/dev/null 2>&1; then
    echo "Target user not found: $TARGET_USER" >&2
    exit 2
fi
TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
if [ -z "$TARGET_HOME" ] || [ ! -d "$TARGET_HOME" ]; then
    echo "Could not resolve an existing home for $TARGET_USER" >&2
    exit 2
fi
command -v xauth >/dev/null 2>&1 || {{ echo "xauth is required for display preflight" >&2; exit 2; }}
command -v xrandr >/dev/null 2>&1 || {{ echo "xrandr is required for display preflight" >&2; exit 2; }}
if [ "$(systemctl show sunshine.service --property=LoadState --value 2>/dev/null)" != "loaded" ]; then
    echo "Required service is not loaded: sunshine.service" >&2
    exit 2
fi
loginctl enable-linger "$TARGET_USER" 2>/dev/null || true
{disable_lock}
systemctl enable sunshine.service >/dev/null
if [ "$(systemctl show noland-xorg.service --property=LoadState --value 2>/dev/null)" = "loaded" ]; then
    systemctl mask gdm sddm lightdm 2>/dev/null || true
    systemctl enable noland-xorg.service >/dev/null
    if systemctl cat noland-desktop.service >/dev/null 2>&1; then systemctl enable noland-desktop.service >/dev/null; fi
    if systemctl cat noland-display-mode.service >/dev/null 2>&1; then systemctl enable noland-display-mode.service >/dev/null; fi
    mkdir -p /etc/systemd/system/sunshine.service.d
    if [ ! -f /etc/systemd/system/sunshine.service.d/noland-display.conf ]; then
    cat > /etc/systemd/system/sunshine.service.d/noland-display.conf <<'EOF'
[Unit]
Requires=noland-xorg.service
After=noland-xorg.service network-online.target
Wants=network-online.target

[Service]
Environment=XAUTHORITY=/etc/X11/.Xauthority-noland
EOF
    fi
fi
systemctl daemon-reload
CANONICAL_XAUTH=/etc/X11/.Xauthority-noland
USER_XAUTH="$TARGET_HOME/.Xauthority"
DISPLAY_XAUTH=""
for candidate in "$CANONICAL_XAUTH" "$USER_XAUTH"; do
    if [ ! -s "$candidate" ] || ! sudo -u "$TARGET_USER" test -r "$candidate"; then
        continue
    fi
    XAUTH_ENTRIES=$(sudo -u "$TARGET_USER" xauth -f "$candidate" list 2>/dev/null || true)
    if [ -n "$XAUTH_ENTRIES" ]; then
        DISPLAY_XAUTH="$candidate"
        break
    fi
done
if [ -z "$DISPLAY_XAUTH" ]; then
    echo "No non-empty, readable Xauthority with entries was found at $CANONICAL_XAUTH or $USER_XAUTH" >&2
    exit 2
fi
echo "REBOOT_PREFLIGHT_OK user=$TARGET_USER home=$TARGET_HOME xauthority=$DISPLAY_XAUTH""#,
            target_user = shell::quote(target_user),
            disable_lock = DISABLE_SCREEN_LOCK_SH,
        );
        let command = format!("sudo bash -lc {}", shell::quote(&script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(30)).await?;

        if output.status_code != 0 || !output.stdout.contains("REBOOT_PREFLIGHT_OK") {
            return Err(AppError::Provisioning(format!(
                "Restart preflight failed: stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        info!(
            target_user = target_user,
            preflight = %output.stdout.trim(),
            "Restart preflight passed"
        );
        Ok(())
    }

    async fn ensure_noland_xorg(remote: &RemoteExec) -> AppResult<()> {
        let script = r#"set -euo pipefail
if [ "$(systemctl show noland-xorg.service --property=LoadState --value 2>/dev/null)" != "loaded" ]; then
    echo NOLAND_XORG_NOT_INSTALLED
    exit 0
fi
systemctl daemon-reload
systemctl enable noland-xorg.service
systemctl start noland-xorg.service
for attempt in $(seq 1 20); do
    if systemctl is-enabled --quiet noland-xorg.service && systemctl is-active --quiet noland-xorg.service; then
        echo NOLAND_XORG_READY
        exit 0
    fi
    sleep 1
done
echo NOLAND_XORG_NOT_READY
systemctl status noland-xorg.service --no-pager 2>/dev/null || true
journalctl -u noland-xorg.service -b --no-pager -n 80 2>/dev/null || true
tail -80 /var/log/Xorg.0.log 2>/dev/null || true
exit 1"#;
        let command = format!("sudo bash -lc {}", shell::quote(script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(35)).await?;

        if output.stdout.contains("NOLAND_XORG_NOT_INSTALLED") {
            info!("Custom Noland Xorg is not installed; preserving the host display-manager path");
            return Ok(());
        }
        if output.status_code != 0 || !output.stdout.contains("NOLAND_XORG_READY") {
            return Err(AppError::Provisioning(format!(
                "noland-xorg could not be enabled and started after restart. stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        info!("noland-xorg is enabled and active after restart");
        Ok(())
    }

    async fn ensure_display_mode(remote: &RemoteExec) -> AppResult<()> {
        let script = r#"set -euo pipefail
if ! systemctl cat noland-display-mode.service >/dev/null 2>&1; then
    echo NOLAND_DISPLAY_MODE_NOT_INSTALLED
    exit 0
fi
systemctl enable noland-display-mode.service >/dev/null
systemctl restart noland-display-mode.service
if ! systemctl is-active --quiet noland-display-mode.service; then
    systemctl status noland-display-mode.service --no-pager 2>/dev/null || true
    journalctl -u noland-display-mode.service -b --no-pager -n 80 2>/dev/null || true
    exit 1
fi
echo NOLAND_DISPLAY_MODE_READY"#;
        let command = format!("sudo bash -lc {}", shell::quote(script));
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(50)).await?;
        if output.status_code != 0 {
            return Err(AppError::Provisioning(format!(
                "The selected display mode could not be restored after restart. stdout: {} | stderr: {}",
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }
        info!("Persistent display mode restored after restart");
        Ok(())
    }

    async fn recover_sunshine(
        remote: &RemoteExec,
        target_user: &str,
        display_xauthority: &str,
    ) -> AppResult<()> {
        let script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
DISPLAY_XAUTH={display_xauthority}
if [ ! -s "$DISPLAY_XAUTH" ]; then
    echo SUNSHINE_POST_RESTART_FAIL
    echo "XAUTHORITY_MISSING_OR_EMPTY=$DISPLAY_XAUTH"
    exit 1
fi
if ! sudo -u "$TARGET_USER" env DISPLAY=:0 XAUTHORITY="$DISPLAY_XAUTH" xrandr --listmonitors >/dev/null 2>&1; then
    echo SUNSHINE_POST_RESTART_FAIL
    echo "DISPLAY_NOT_READY xauthority=$DISPLAY_XAUTH"
    exit 1
fi
systemctl enable sunshine.service >/dev/null
systemctl restart sunshine.service
sleep 2
WEB_OK=0
RTSP_OK=0
for attempt in $(seq 1 30); do
    if curl -k -s --connect-timeout 5 https://localhost:47990/pin >/dev/null 2>&1 || curl -k -s --connect-timeout 5 https://127.0.0.1:47990/pin >/dev/null 2>&1; then
        WEB_OK=1
    fi
    if ss -ltn 2>/dev/null | grep -Eq ':48010[[:space:]]'; then
        RTSP_OK=1
    fi
    if [ "$WEB_OK" = "1" ] && [ "$RTSP_OK" = "1" ]; then
        break
    fi
    sleep 1
done
PROC_COUNT=$(pgrep -u "$TARGET_USER" -x sunshine 2>/dev/null | wc -l | tr -d '[:space:]' || true)
INVOCATION_ID=$(systemctl show sunshine.service --property=InvocationID --value 2>/dev/null || true)
if [ -n "$INVOCATION_ID" ]; then
    CURRENT_LOGS=$(journalctl "_SYSTEMD_INVOCATION_ID=$INVOCATION_ID" --no-pager -n 120 2>/dev/null || true)
else
    CURRENT_LOGS=$(journalctl -u sunshine.service -b --no-pager -n 120 2>/dev/null || true)
fi
if [ "$PROC_COUNT" != "1" ] || [ "$WEB_OK" != "1" ] || [ "$RTSP_OK" != "1" ] || ! systemctl is-active --quiet sunshine.service; then
    echo SUNSHINE_POST_RESTART_FAIL
    echo "PROC_COUNT=$PROC_COUNT WEB_OK=$WEB_OK RTSP_OK=$RTSP_OK"
    systemctl status sunshine.service --no-pager 2>/dev/null || true
    printf '%s\n' "$CURRENT_LOGS"
    echo '--- listening ports ---'
    ss -ltnp 2>/dev/null | grep -E ':(47990|48010)[[:space:]]' || true
    exit 1
fi
if printf '%s\n' "$CURRENT_LOGS" | grep -Eqi 'Unable to open display|Failed to (open|create).*display|Could not open.*display'; then
    echo SUNSHINE_POST_RESTART_FAIL
    echo OPEN_DISPLAY_ERROR_IN_CURRENT_INVOCATION
    printf '%s\n' "$CURRENT_LOGS"
    exit 1
fi
echo "SUNSHINE_POST_RESTART_OK web=47990 rtsp=48010 xauthority=$DISPLAY_XAUTH""#,
            target_user = shell::quote(target_user),
            display_xauthority = shell::quote(display_xauthority),
        );
        let restart_command = format!("sudo bash -lc {}", shell::quote(&script));
        let output = Self::probe_ssh(remote, &restart_command, Duration::from_secs(90)).await?;
        if output.status_code != 0 || !output.stdout.contains("SUNSHINE_POST_RESTART_OK") {
            return Err(AppError::Provisioning(format!(
                "Sunshine failed post-restart recovery using DISPLAY=:0 and XAUTHORITY={}. stdout: {} | stderr: {}",
                display_xauthority,
                output.stdout.trim(),
                filtered_probe_stderr(&output.stderr)
            )));
        }

        info!(
            target_user = target_user,
            display_xauthority = display_xauthority,
            "Sunshine recovered after restart; web and RTSP listeners are ready"
        );
        Ok(())
    }

    async fn ensure_audio_ready(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
        let target_uid = Self::resolve_user_uid(remote, target_user).await?;
        let runtime_dir = format!("/run/user/{target_uid}");
        let bus_path = format!("{runtime_dir}/bus");

        let check_command = format!(
            "sudo {}",
            shell::bash_lc(&format!(
                "TARGET_USER={target_user}; RUNTIME_DIR={runtime_dir}; BUS_PATH={bus_path}; mkdir -p \"$RUNTIME_DIR\"; chown \"$TARGET_USER:$(id -gn $TARGET_USER)\" \"$RUNTIME_DIR\"; chmod 700 \"$RUNTIME_DIR\"; run_user() {{ sudo -u \"$TARGET_USER\" env XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" DBUS_SESSION_BUS_ADDRESS=unix:path=\"$BUS_PATH\" \"$@\"; }}; PW=$(run_user systemctl --user is-active pipewire 2>/dev/null || true); PWP=$(run_user systemctl --user is-active pipewire-pulse 2>/dev/null || true); WP=$(run_user systemctl --user is-active wireplumber 2>/dev/null || true); SINK_OK=0; if run_user pactl list short sinks 2>/dev/null | grep -Eq \"^[0-9]+[[:space:]]+sunshine_audio([[:space:]]|$)\"; then SINK_OK=1; fi; if [ -S \"$BUS_PATH\" ]; then echo \"session_bus=1\"; else echo \"session_bus=0\"; fi; if [ \"$PW\" = \"active\" ] && [ \"$PWP\" = \"active\" ] && [ \"$WP\" = \"active\" ] && [ \"$SINK_OK\" = \"1\" ]; then echo AUDIO_READY; else echo AUDIO_NOT_READY; echo \"pipewire=$PW\"; echo \"pipewire_pulse=$PWP\"; echo \"wireplumber=$WP\"; echo \"sunshine_audio_sink=$SINK_OK\"; fi",
                target_user = shell::quote(target_user),
                runtime_dir = shell::quote(&runtime_dir),
                bus_path = shell::quote(&bus_path),
            ))
        );

        let first = Self::probe_ssh(remote, &check_command, Duration::from_secs(20)).await?;
        if first.status_code == 0 && first.stdout.contains("AUDIO_READY") {
            info!(
                target_user = target_user,
                "Post-restart audio stack is ready"
            );
            return Ok(());
        }

        warn!(
            target_user = target_user,
            stdout = %first.stdout.trim(),
            stderr = %filtered_probe_stderr(&first.stderr),
            "Post-restart audio stack not ready; attempting one recovery pass"
        );

        let repair_script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
RUNTIME_DIR={runtime_dir}
BUS_PATH={bus_path}
TARGET_HOME=$(getent passwd "$TARGET_USER" | cut -d: -f6)
mkdir -p "$RUNTIME_DIR"
chown "$TARGET_USER:$(id -gn "$TARGET_USER")" "$RUNTIME_DIR"
chmod 700 "$RUNTIME_DIR"
run_user() {{
    sudo -u "$TARGET_USER" env \
        HOME="$TARGET_HOME" \
        XDG_RUNTIME_DIR="$RUNTIME_DIR" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=$BUS_PATH" \
        "$@"
}}
run_user mkdir -p "$TARGET_HOME/.config/pipewire/pipewire.conf.d"
cat > "$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf" <<'EOF'
context.objects = [
    {{
        factory = adapter
        args = {{
            factory.name = support.null-audio-sink
            node.name = sunshine_audio
            node.description = "Noland Audio"
            media.class = "Audio/Sink"
            audio.position = [ FL FR ]
            monitor.channel-volumes = true
            monitor.passthrough = true
            adapter.auto-port-config = {{
                mode = dsp
                monitor = true
                position = preserve
            }}
        }}
    }}
]
EOF
chown "$TARGET_USER:$(id -gn "$TARGET_USER")" "$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf"
rm -f "$TARGET_HOME/.config/pipewire/pipewire-pulse.conf.d/20-sunshine-audio.conf"
run_user systemctl --user start pipewire.service pipewire-pulse.service wireplumber.service || true
sleep 2
if ! run_user pactl list short sinks 2>/dev/null | grep -Eq '^[0-9]+[[:space:]]+sunshine_audio([[:space:]]|$)'; then
    run_user pactl load-module module-null-sink \
        sink_name=sunshine_audio \
        sink_properties=device.description=Noland-Audio \
        rate=48000 channels=2 >/dev/null
fi
run_user pactl set-default-sink sunshine_audio
"#,
            target_user = shell::quote(target_user),
            runtime_dir = shell::quote(&runtime_dir),
            bus_path = shell::quote(&bus_path),
        );
        let repair_command = format!("sudo bash -lc {}", shell::quote(&repair_script));
        let _ = Self::probe_ssh(remote, &repair_command, Duration::from_secs(25)).await?;

        let second = Self::probe_ssh(remote, &check_command, Duration::from_secs(20)).await?;
        if second.status_code == 0 && second.stdout.contains("AUDIO_READY") {
            info!(
                target_user = target_user,
                "Post-restart audio stack recovered successfully"
            );
            return Ok(());
        }

        let diag_command = format!(
            "sudo {}",
            shell::bash_lc(&format!(
                "TARGET_USER={target_user}; RUNTIME_DIR={runtime_dir}; BUS_PATH={bus_path}; TARGET_HOME=$(getent passwd \"$TARGET_USER\" | cut -d: -f6); run_user() {{ sudo -u \"$TARGET_USER\" env XDG_RUNTIME_DIR=\"$RUNTIME_DIR\" DBUS_SESSION_BUS_ADDRESS=unix:path=\"$BUS_PATH\" \"$@\"; }}; echo --- session-bus ---; test -S \"$BUS_PATH\" && echo BUS_OK || echo BUS_MISSING; echo --- user-audio-status ---; run_user systemctl --user status pipewire pipewire-pulse wireplumber --no-pager 2>/dev/null || true; echo --- pactl-info ---; run_user pactl info 2>/dev/null || true; echo --- sinks ---; run_user pactl list short sinks 2>/dev/null || true; echo --- sources ---; run_user pactl list short sources 2>/dev/null || true; echo --- sunshine-audio-dropin ---; if [ -f \"$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf\" ]; then run_user cat \"$TARGET_HOME/.config/pipewire/pipewire.conf.d/70-noland-sunshine-audio.conf\"; else echo MISSING; fi",
                target_user = shell::quote(target_user),
                runtime_dir = shell::quote(&runtime_dir),
                bus_path = shell::quote(&bus_path),
            ))
        );
        let diag = Self::probe_ssh(remote, &diag_command, Duration::from_secs(20)).await?;

        Err(AppError::Provisioning(format!(
            "Post-restart audio recovery failed: sunshine_audio sink missing or audio services inactive. check1: {} | check2: {} | diagnostics: {} | stderr: {}",
            first.stdout.trim(),
            second.stdout.trim(),
            diag.stdout.trim(),
            filtered_probe_stderr(&diag.stderr)
        )))
    }

    async fn wait_for_user_display_ready(
        remote: &RemoteExec,
        target_user: &str,
    ) -> AppResult<String> {
        const DISPLAY_ATTEMPTS: usize = 30;
        const DISPLAY_INTERVAL: Duration = Duration::from_secs(2);
        let target_home = Self::resolve_user_home(remote, target_user).await?;
        let script = format!(
            r#"set -euo pipefail
TARGET_USER={target_user}
USER_XAUTH={user_xauth}
for candidate in /etc/X11/.Xauthority-noland "$USER_XAUTH"; do
    if [ "$candidate" = "/etc/X11/.Xauthority-noland" ]; then
        chmod 0644 "$candidate" 2>/dev/null || true
    fi
    if [ ! -s "$candidate" ] || ! sudo -u "$TARGET_USER" test -r "$candidate"; then
        continue
    fi
    if command -v timeout >/dev/null 2>&1; then
        DISPLAY_COMMAND=(timeout -s 9 5s xrandr --listmonitors)
    else
        DISPLAY_COMMAND=(xrandr --listmonitors)
    fi
    if sudo -u "$TARGET_USER" env DISPLAY=:0 XAUTHORITY="$candidate" "${{DISPLAY_COMMAND[@]}}" >/dev/null 2>&1; then
        echo "DISPLAY_XAUTHORITY=$candidate"
        exit 0
    fi
done
exit 1"#,
            target_user = shell::quote(target_user),
            user_xauth = shell::quote(&format!("{target_home}/.Xauthority")),
        );
        let command = format!("sudo bash -lc {}", shell::quote(&script));

        for attempt in 1..=DISPLAY_ATTEMPTS {
            match Self::probe_ssh(remote, &command, Duration::from_secs(30)).await {
                Ok(output) if output.status_code == 0 => {
                    if let Some(xauthority) = parse_display_xauthority(&output.stdout) {
                        info!(
                            attempt = attempt,
                            target_user = target_user,
                            xauthority = xauthority,
                            "User display became ready after restart"
                        );
                        return Ok(xauthority);
                    }
                    warn!(
                        attempt = attempt,
                        target_user = target_user,
                        "Display probe succeeded without reporting its Xauthority path"
                    );
                }
                Ok(output) => {
                    warn!(
                        attempt = attempt,
                        target_user = target_user,
                        status_code = output.status_code,
                        stderr = %filtered_probe_stderr(&output.stderr),
                        "Waiting for user display path to become ready after restart"
                    );
                }
                Err(error) => {
                    warn!(
                        attempt = attempt,
                        target_user = target_user,
                        error = %error,
                        "Waiting for user display path to become ready after restart"
                    );
                }
            }
            sleep(DISPLAY_INTERVAL).await;
        }

        Err(AppError::Timeout(format!(
            "User display did not become ready after restart using DISPLAY=:0 with /etc/X11/.Xauthority-noland or {target_home}/.Xauthority"
        )))
    }

    async fn resolve_user_home(remote: &RemoteExec, target_user: &str) -> AppResult<String> {
        let command = format!("getent passwd {} | cut -d: -f6", target_user);
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(30)).await?;
        if output.status_code == 0 {
            let home = output.stdout.trim();
            if !home.is_empty() {
                return Ok(home.to_string());
            }
        }
        Err(AppError::Provisioning(format!(
            "Could not resolve home directory for user {}",
            target_user
        )))
    }

    async fn resolve_user_uid(remote: &RemoteExec, target_user: &str) -> AppResult<u32> {
        let command = format!("id -u {}", target_user);
        let output = Self::probe_ssh(remote, &command, Duration::from_secs(30)).await?;
        if output.status_code == 0 {
            return output.stdout.trim().parse::<u32>().map_err(|error| {
                AppError::Provisioning(format!(
                    "Failed to parse UID for {}: {}",
                    target_user, error
                ))
            });
        }
        Err(AppError::Provisioning(format!(
            "Could not resolve UID for user {}",
            target_user
        )))
    }

    async fn probe_ssh(
        remote: &RemoteExec,
        command: &str,
        timeout: Duration,
    ) -> AppResult<crate::services::remote_exec::ExecOutput> {
        let remote = remote.clone();
        let command = command.to_string();
        tokio::task::spawn_blocking(move || remote.ssh(&command, timeout))
            .await
            .map_err(|error| AppError::Command(format!("join failure: {error}")))?
    }
}

const DISPLAY_XAUTHORITY_PREFIX: &str = "DISPLAY_XAUTHORITY=";

fn parse_display_xauthority(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let path = line.trim().strip_prefix(DISPLAY_XAUTHORITY_PREFIX)?.trim();
        (!path.is_empty()).then(|| path.to_string())
    })
}

fn filtered_probe_stderr(stderr: &str) -> String {
    let filtered = stderr
        .lines()
        .filter(|line| !(line.contains("Permanently added") && line.contains("known hosts")))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    if filtered.is_empty() {
        stderr.trim().to_string()
    } else {
        filtered
    }
}

#[cfg(test)]
mod tests {
    use super::{filtered_probe_stderr, parse_display_xauthority};

    #[test]
    fn known_hosts_warning_is_filtered_from_probe_stderr() {
        let stderr = "Warning: Permanently added '[1.2.3.4]:22' (ED25519) to the list of known hosts.\nConnection refused";
        assert_eq!(filtered_probe_stderr(stderr), "Connection refused");
    }

    #[test]
    fn parses_display_xauthority_marker() {
        assert_eq!(
            parse_display_xauthority("noise\nDISPLAY_XAUTHORITY=/etc/X11/.Xauthority-noland\n")
                .as_deref(),
            Some("/etc/X11/.Xauthority-noland")
        );
        assert_eq!(parse_display_xauthority("DISPLAY_XAUTHORITY=\n"), None);
    }
}
