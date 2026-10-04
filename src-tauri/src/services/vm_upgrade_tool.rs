//! Install the desktop distro-upgrade helper without starting an upgrade.
use std::time::Duration;

use base64::Engine;

use super::remote_exec::RemoteExec;
use crate::{
    errors::{AppError, AppResult},
    utils::shell,
};

const UPGRADE: &str = include_str!("../../scripts/upgrade_noland_vm.sh");
const DISPLAY: &str = include_str!("../../scripts/change_display_resolution.py");
const INSTALL: &str = include_str!("../../scripts/install_vm_upgrade_tool.sh");

fn install_command(sudo: &str, target_user: &str) -> String {
    let encode = |script: &str| {
        shell::quote(
            &base64::engine::general_purpose::STANDARD.encode(script.replace("\r\n", "\n")),
        )
    };
    format!(
        "set -e; staging=$(mktemp -d); trap 'rm -rf \"$staging\"' EXIT; \
         printf %s {upgrade} | base64 -d > \"$staging/upgrade.sh\"; \
         printf %s {install} | base64 -d > \"$staging/install.sh\"; \
         printf %s {display} | base64 -d > \"$staging/display.py\"; \
         {sudo}bash \"$staging/install.sh\" {user} \"$staging/upgrade.sh\" \"$staging/display.py\"",
        upgrade = encode(UPGRADE),
        install = encode(INSTALL),
        display = encode(DISPLAY),
        user = shell::quote(target_user),
    )
}

pub async fn install(remote: &RemoteExec, target_user: &str) -> AppResult<()> {
    let command = install_command(&remote.sudo_prefix(), target_user);
    let remote = remote.clone();
    let output = tokio::task::spawn_blocking(move || remote.ssh(&command, Duration::from_secs(60)))
        .await
        .map_err(|error| {
            AppError::Command(format!("Upgrade helper install join failure: {error}"))
        })??;
    if output.status_code != 0 || !output.stdout.contains("NOLAND_UPGRADE_TOOL_READY") {
        return Err(AppError::Provisioning(format!(
            "Failed installing Desktop/tools upgrade helper: {}",
            output.stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn installer_command_and_embedded_scripts_are_valid_bash() {
        for script in [
            super::UPGRADE.to_string(),
            super::INSTALL.to_string(),
            super::install_command("sudo ", "user's account"),
        ] {
            assert!(std::process::Command::new("bash")
                .args(["-n", "-c", &script])
                .status()
                .expect("bash available")
                .success());
        }
    }
}
