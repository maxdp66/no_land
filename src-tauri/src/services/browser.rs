//! Installs Brave as the remote VM's web browser, replacing Google Chrome.

use std::time::Duration;

use base64::Engine;
use tracing::{info, warn};

use crate::utils::shell;

use super::remote_exec::RemoteExec;

const INSTALL_SCRIPT: &str = include_str!("../../scripts/install_brave_browser.sh");

/// Best effort: a failed browser install never blocks streaming setup.
pub async fn ensure_brave(remote: &RemoteExec, target_user: &str) {
    // Windows checkouts may embed the script with CRLF line endings.
    let script = INSTALL_SCRIPT.replace("\r\n", "\n");
    let encoded = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    let command = format!(
        "printf %s {encoded} | base64 -d > /tmp/noland-install-brave.sh && {sudo}bash /tmp/noland-install-brave.sh {user}; status=$?; rm -f /tmp/noland-install-brave.sh; exit $status",
        encoded = shell::quote(&encoded),
        sudo = remote.sudo_prefix(),
        user = shell::quote(target_user),
    );

    let remote = remote.clone();
    let result =
        tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(900))).await;
    match result {
        Ok(Ok(output))
            if output.status_code == 0 && output.stdout.contains("NOLAND_BRAVE_READY") =>
        {
            info!(details = %output.stdout.trim(), "Brave browser ready");
        }
        Ok(Ok(output)) => warn!(
            status = output.status_code,
            stdout = %output.stdout.trim(),
            stderr = %output.stderr.trim(),
            "Brave browser install failed"
        ),
        Ok(Err(error)) => warn!(%error, "Brave browser install failed to run"),
        Err(error) => warn!(%error, "Brave browser install join failure"),
    }
}
