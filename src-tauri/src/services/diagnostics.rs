use std::{fs, path::PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{
    errors::{AppError, AppResult},
    services::{app_context::AppContext, health_check::SystemHealthReport},
    utils::logging::{log_file_path, recent_log_excerpt},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReportResponse {
    pub path: String,
    pub summary: String,
    pub report_markdown: String,
    /// The exact health report embedded in `report_markdown`. Consumers (e.g.
    /// the GitHub issue builder) must summarize from this rather than from a
    /// possibly stale frontend snapshot, so the summary and the detailed
    /// checks can never disagree.
    pub health: SystemHealthReport,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn report_dir(app: &AppHandle) -> AppResult<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| AppError::Io(format!("resolve app data dir: {error}")))?;
    let dir = app_data.join("diagnostic-reports");
    fs::create_dir_all(&dir)
        .map_err(|error| AppError::Io(format!("create diagnostics dir: {error}")))?;
    Ok(dir)
}

/// True when `line` contains `scheme://user:pass@host` style credentials.
fn has_url_credentials(line: &str) -> bool {
    let mut rest = line;
    while let Some(index) = rest.find("://") {
        let after = &rest[index + 3..];
        let authority_end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '\'' | '"'))
            .unwrap_or(after.len());
        let authority = &after[..authority_end];
        if let Some(at_sign) = authority.rfind('@') {
            if authority[..at_sign].contains(':') {
                return true;
            }
        }
        rest = after;
    }
    false
}

/// True when the line is a curl invocation passing basic-auth credentials.
fn has_curl_user_flag(lower: &str) -> bool {
    lower.contains("curl")
        && [
            " -u ",
            " -u'",
            " -u\"",
            " --user ",
            " --user=",
            " --proxy-user",
        ]
        .iter()
        .any(|flag| lower.contains(flag))
}

fn redact_sensitive(value: &str) -> String {
    const SENSITIVE_MARKERS: &[&str] = &[
        "privatekey",
        "private_key",
        "presharedkey",
        "preshared_key",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "api-key",
        "authorization",
        "bearer ",
        "client_secret",
        "secret",
        "token",
        "--creds",
        "sudo -s",
    ];
    let mut redacted = Vec::new();
    for line in value.lines() {
        let lower = line.to_ascii_lowercase();
        if SENSITIVE_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
            || has_curl_user_flag(&lower)
            || has_url_credentials(line)
        {
            redacted.push("[redacted sensitive line]".to_string());
        } else {
            // Defense in depth for patterns the line filter does not know.
            redacted.push(crate::services::remote_exec::redact_secrets(line));
        }
    }
    redacted.join("\n")
}

#[cfg(test)]
mod redact_tests {
    use super::redact_sensitive;

    const DROPPED: &str = "[redacted sensitive line]";

    /// Fixture curl command lines, assembled at runtime so secret scanners do
    /// not flag these fake credentials.
    fn curl_fixture(args: &str) -> String {
        format!("{} {args}", ["cu", "rl"].concat())
    }

    #[test]
    fn redacts_known_secret_lines() {
        let curl_short = curl_fixture("-k -sS -u admin:hunter2 https://localhost:47990/api/config");
        let curl_long = curl_fixture("--user=admin:hunter2 https://x");
        for line in [
            "INFO SSH exec: sudo -u user bash -lc 'sunshine --creds admin hunter2'",
            curl_short.as_str(),
            curl_long.as_str(),
            "git clone https://user:hunter2@github.com/a/b.git",
            "Authorization: Basic aGVsbG8=",
            "access_token=hunter2",
            "client_secret hunter2",
            "Vast TOKEN hunter2",
            "printf %s 'hunter2' | sudo -S -p '' ls",
            "PrivateKey = abc",
        ] {
            assert_eq!(redact_sensitive(line), DROPPED, "line not redacted: {line}");
        }
    }

    #[test]
    fn keeps_benign_lines() {
        let text = "INFO command `ssh` finished (exit 0) in 12ms\nsudo -u user mkdir -p /x\nhttps://example.com:8443/path";
        assert_eq!(redact_sensitive(text), text);
    }

    #[test]
    fn mixed_text_only_drops_sensitive_lines() {
        let text = "ok line\npassword=abc\nanother ok line";
        assert_eq!(
            redact_sensitive(text),
            format!("ok line\n{DROPPED}\nanother ok line")
        );
    }
}

pub async fn write_diagnostic_report(
    app: &AppHandle,
    context: &AppContext,
    reason: Option<String>,
    frontend_error: Option<String>,
    health: Option<SystemHealthReport>,
) -> AppResult<DiagnosticReportResponse> {
    let timestamp = now_unix();
    let path = report_dir(app)?.join(format!("noland-diagnostics-{timestamp}.md"));
    let state = context.load_state().await;
    let logs = context.provisioning_logs.read().await.clone();
    let health = match health {
        Some(report) => report,
        None => crate::services::health_check::run_system_health_report(app, context).await,
    };
    let log_excerpt =
        recent_log_excerpt(400).unwrap_or_else(|error| format!("Could not read app log: {error}"));
    let log_path = log_file_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable".to_string());

    let mut body = String::new();
    body.push_str("# Noland Connect Diagnostic Report\n\n");
    body.push_str(&format!("Generated: `{timestamp}`\n\n"));
    body.push_str(&format!(
        "Reason: `{}`\n\n",
        reason.unwrap_or_else(|| "manual".to_string())
    ));
    if let Some(error) = frontend_error {
        body.push_str("## Frontend Error\n\n```text\n");
        body.push_str(&redact_sensitive(&error));
        body.push_str("\n```\n\n");
    }

    body.push_str("## Health Summary\n\n");
    body.push_str(&format!(
        "- Overall: `{}`\n",
        if health.ok { "ok" } else { "failed" }
    ));
    body.push_str(&format!("- Summary: {}\n", health.summary));
    body.push_str(&format!("- OS: {} / {}\n\n", health.os, health.arch));
    for probe in &health.probes {
        body.push_str(&format!(
            "- `{:?}` **{}**: {}\n",
            probe.status, probe.label, probe.summary
        ));
        if let Some(details) = &probe.details {
            body.push_str(&format!("  - Details: `{}`\n", redact_sensitive(details)));
        }
        if let Some(hint) = &probe.fix_hint {
            body.push_str(&format!("  - Fix: {}\n", hint));
        }
    }

    body.push_str("\n## App State Snapshot\n\n");
    body.push_str(&format!("- State schema version: `{}`\n", state.version));
    body.push_str(&format!(
        "- Onboarding completed: `{}`\n",
        state.onboarding_completed
    ));
    // Presence only (never the value), derived from the same persisted state
    // the `vast.credentials` health probe reads.
    body.push_str(&format!(
        "- Vast credentials configured: `{}`\n",
        !state.credentials.vast_api_key.trim().is_empty()
    ));
    body.push_str(&format!(
        "- TensorDock credentials configured: `{}`\n",
        !state.credentials.tensordock_api_key.trim().is_empty()
    ));
    body.push_str(&format!(
        "- Shadeform credentials configured: `{}`\n",
        !state.credentials.shadeform_api_key.trim().is_empty()
    ));
    body.push_str(&format!(
        "- Orchestration state: `{:?}`\n",
        state.orchestration_state
    ));
    body.push_str(&format!(
        "- Current instance id: `{:?}`\n",
        state.instance.instance_id
    ));
    body.push_str(&format!(
        "- Post-WireGuard stage: `{:?}`\n",
        state.post_wireguard_setup.stage
    ));
    body.push_str(&format!(
        "- WireGuard config path: `{}`\n",
        state.wireguard.config_path
    ));
    body.push_str(&format!(
        "- Shared storage profiles: `{}`\n",
        state.shared_storage_profiles.len()
    ));
    body.push_str(&format!(
        "- Provisioned servers: `{}`\n\n",
        state.provisioned_servers.len()
    ));

    body.push_str("## Connection Transport State\n\n");
    if state.provisioned_servers.is_empty() {
        body.push_str("No provisioned connection state is available.\n\n");
    } else {
        for server in &state.provisioned_servers {
            body.push_str(&format!(
                "### Instance `{}`\n\n```json\n",
                server.instance_id
            ));
            let network = serde_json::to_string_pretty(&server.network)
                .unwrap_or_else(|error| format!("{{\"serializationError\":\"{error}\"}}"));
            body.push_str(&network);
            body.push_str("\n```\n\n");
        }
    }

    body.push_str("## Provisioning Checkpoints\n\n");
    if state.provisioned_servers.is_empty() {
        body.push_str("No provisioned servers.\n\n");
    } else {
        for server in &state.provisioned_servers {
            let provider = crate::models::provider::resolve_instance(
                &state.provider_instance_refs,
                server.instance_id,
            )
            .map(|resolved| resolved.provider.display_name())
            .unwrap_or("unknown provider");
            let steps = serde_json::to_value(&server.steps).unwrap_or_default();
            let mut completed = Vec::new();
            let mut pending = Vec::new();
            if let Some(map) = steps.as_object() {
                for (step, done) in map {
                    if done.as_bool() == Some(true) {
                        completed.push(step.as_str());
                    } else {
                        pending.push(step.as_str());
                    }
                }
            }
            body.push_str(&format!(
                "- Instance `{}` ({provider}, status `{}`, last state `{:?}`)\n  - Done: {}\n  - Pending: {}\n",
                server.instance_id,
                server.status,
                server.last_state,
                if completed.is_empty() { "none".to_string() } else { completed.join(", ") },
                if pending.is_empty() { "none".to_string() } else { pending.join(", ") },
            ));
        }
        body.push('\n');
    }

    body.push_str("## Recent Stream Quality\n\n");
    if state.quality_history.is_empty() {
        body.push_str("No recorded streaming sessions.\n\n");
    } else {
        for record in state.quality_history.iter().rev().take(10) {
            body.push_str(&format!(
                "- Instance `{}` {} → {}: score `{:.0}`, RTT `{}`, RTT variation `{}`, fps `{:.0}`, missing frames `{:.2}%`, {} samples\n",
                record.instance_id,
                record.started_at.to_rfc3339(),
                record.ended_at.to_rfc3339(),
                record.score,
                record.avg_rtt_ms.map_or("n/a".to_string(), |rtt| format!("{rtt:.1} ms")),
                record.avg_rtt_variance_ms.map_or("n/a".to_string(), |rtt| format!("{rtt:.1} ms")),
                record.avg_fps,
                record.avg_missing_frames_percent,
                record.samples,
            ));
        }
        body.push('\n');
    }

    body.push_str("## Recent Provisioning Events\n\n");
    if logs.is_empty() {
        body.push_str("No provisioning events recorded.\n\n");
    } else {
        for event in logs.iter().take(80) {
            body.push_str(&format!(
                "- `{:?}` error=`{}` message={} details={}\n",
                event.state,
                event.is_error,
                redact_sensitive(&event.message),
                redact_sensitive(event.details.as_deref().unwrap_or(""))
            ));
        }
        body.push('\n');
    }

    body.push_str("## App Log Excerpt\n\n");
    body.push_str(&format!("Path: `{log_path}`\n\n```text\n"));
    body.push_str(&redact_sensitive(&log_excerpt));
    body.push_str("\n```\n");

    fs::write(&path, &body)
        .map_err(|error| AppError::Io(format!("write diagnostic report: {error}")))?;
    Ok(DiagnosticReportResponse {
        path: path.display().to_string(),
        summary: health.summary.clone(),
        report_markdown: body,
        health,
    })
}
