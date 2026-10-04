//! Lets users stream with their own Moonlight install instead of the embedded client.
//!
//! The managed WireGuard tunnel is an OS-level adapter, so while it is up any client on
//! this computer can reach Sunshine at the tunnel address. These commands expose the
//! connection details, approve a Moonlight PIN through the Sunshine API, and launch a
//! user-chosen Moonlight executable with its command-line `pair` / `stream` actions.

use std::{
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use rand::RngCore;
use serde::Serialize;
use tauri::State;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    errors::{AppError, AppResult, FrontendError},
    models::app_state::PersistedAppState,
    services::{
        app_context::AppContext,
        post_wireguard_setup::{authorize_sunshine_pin, SUNSHINE_API_PORT, TUNNEL_HOST},
    },
};

const EXTERNAL_CLIENT_NAME: &str = "Moonlight (custom install)";
const TUNNEL_PROBE_PORT: u16 = 47989;
const TUNNEL_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
const PAIRING_DEADLINE: Duration = Duration::from_secs(45);
const PAIRING_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalMoonlightPort {
    pub port: u16,
    pub protocol: &'static str,
    pub purpose: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalMoonlightConnectionInfo {
    pub host: String,
    pub web_ui_url: String,
    pub sunshine_username: String,
    pub sunshine_password: String,
    pub ports: Vec<ExternalMoonlightPort>,
    pub tunnel_reachable: bool,
    pub active_instance_id: Option<u64>,
    pub configured_executable_path: Option<String>,
    pub detected_executable_path: Option<String>,
}

fn gamestream_ports() -> Vec<ExternalMoonlightPort> {
    vec![
        ExternalMoonlightPort {
            port: 47984,
            protocol: "TCP",
            purpose: "HTTPS (paired requests)",
        },
        ExternalMoonlightPort {
            port: 47989,
            protocol: "TCP",
            purpose: "HTTP (discovery and pairing)",
        },
        ExternalMoonlightPort {
            port: 47990,
            protocol: "TCP",
            purpose: "Sunshine web UI",
        },
        ExternalMoonlightPort {
            port: 48010,
            protocol: "TCP",
            purpose: "RTSP session setup",
        },
        ExternalMoonlightPort {
            port: 47998,
            protocol: "UDP",
            purpose: "Video",
        },
        ExternalMoonlightPort {
            port: 47999,
            protocol: "UDP",
            purpose: "Control",
        },
        ExternalMoonlightPort {
            port: 48000,
            protocol: "UDP",
            purpose: "Audio",
        },
    ]
}

/// Well-known install locations for the official Moonlight PC client.
fn default_executable_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    #[cfg(target_os = "windows")]
    {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Ok(root) = std::env::var(variable) {
                candidates.push(
                    PathBuf::from(&root)
                        .join("Moonlight Game Streaming")
                        .join("Moonlight.exe"),
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from("/Applications/Moonlight.app"));
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Applications/Moonlight.app"));
        }
    }

    #[cfg(target_os = "linux")]
    {
        for name in ["moonlight", "moonlight-qt"] {
            for dir in ["/usr/bin", "/usr/local/bin"] {
                candidates.push(Path::new(dir).join(name));
            }
        }
        candidates.push(PathBuf::from(
            "/var/lib/flatpak/exports/bin/com.moonlight_stream.Moonlight",
        ));
        if let Some(home) = dirs::home_dir() {
            candidates
                .push(home.join(".local/share/flatpak/exports/bin/com.moonlight_stream.Moonlight"));
        }
    }

    candidates
}

/// Turns a user-chosen path into the file to execute. On macOS an `.app` bundle is
/// accepted and resolved to the binary inside it.
fn resolve_executable(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            let macos_dir = path.join("Contents").join("MacOS");
            let preferred = macos_dir.join("Moonlight");
            if preferred.is_file() {
                return Some(preferred);
            }
            return std::fs::read_dir(&macos_dir)
                .ok()?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|candidate| candidate.is_file());
        }
        return None;
    }
    path.is_file().then(|| path.to_path_buf())
}

fn detect_default_executable() -> Option<PathBuf> {
    default_executable_candidates()
        .into_iter()
        .find(|candidate| resolve_executable(candidate).is_some())
}

fn executable_for_state(state: &PersistedAppState) -> AppResult<PathBuf> {
    let chosen = state
        .external_moonlight
        .executable_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(detect_default_executable)
        .ok_or_else(|| {
            AppError::InvalidInput(
                "Choose your Moonlight app first. No Moonlight install was found in the usual locations."
                    .to_string(),
            )
        })?;

    resolve_executable(&chosen).ok_or_else(|| {
        AppError::InvalidInput(format!(
            "Moonlight was not found at {}. Choose the Moonlight app again.",
            chosen.display()
        ))
    })
}

fn tunnel_reachable() -> bool {
    let Ok(address) = format!("{TUNNEL_HOST}:{TUNNEL_PROBE_PORT}").parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&address, TUNNEL_PROBE_TIMEOUT).is_ok()
}

async fn ensure_tunnel_reachable() -> AppResult<()> {
    let reachable = tokio::task::spawn_blocking(tunnel_reachable)
        .await
        .map_err(|error| AppError::Command(format!("join failure: {error}")))?;
    if reachable {
        Ok(())
    } else {
        Err(AppError::Provisioning(format!(
            "Sunshine is not reachable at {TUNNEL_HOST}. Start the instance from Noland first so the secure tunnel is connected, then try again."
        )))
    }
}

fn spawn_detached(executable: &Path, args: &[&str]) -> AppResult<()> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| {
        AppError::Command(format!(
            "Could not start Moonlight at {}: {error}",
            executable.display()
        ))
    })?;
    info!(executable = %executable.display(), ?args, "Started external Moonlight client");
    // Reap the process when it exits so it does not linger as a zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn random_pin() -> String {
    format!("{:04}", rand::thread_rng().next_u32() % 10_000)
}

fn random_pairing_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_pin(pin: &str) -> AppResult<&str> {
    let pin = pin.trim();
    if pin.len() == 4 && pin.chars().all(|value| value.is_ascii_digit()) {
        Ok(pin)
    } else {
        Err(AppError::InvalidInput(
            "Enter the 4-digit PIN that Moonlight shows.".to_string(),
        ))
    }
}

/// Sunshine only accepts a PIN while a client is waiting in its pairing handshake, so
/// keep offering it while the freshly launched Moonlight connects.
async fn approve_pin_until_client_waits(
    context: &AppContext,
    pin: &str,
    deadline: Duration,
) -> AppResult<()> {
    let state = context.load_state().await;
    let username = state.credentials.app_username.clone();
    let password = state.credentials.app_password.clone();
    let pairing_id = random_pairing_id();
    let started = Instant::now();

    loop {
        match authorize_sunshine_pin(
            TUNNEL_HOST,
            &username,
            &password,
            pin,
            &pairing_id,
            Some(EXTERNAL_CLIENT_NAME),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(AppError::Provisioning(message)) if started.elapsed() < deadline => {
                warn!("Waiting for Moonlight to start pairing: {message}");
                sleep(PAIRING_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[tauri::command]
pub async fn external_moonlight_get_connection_info(
    context: State<'_, AppContext>,
) -> Result<ExternalMoonlightConnectionInfo, FrontendError> {
    let state = context.load_state().await;
    let tunnel_reachable = tokio::task::spawn_blocking(tunnel_reachable)
        .await
        .unwrap_or(false);

    Ok(ExternalMoonlightConnectionInfo {
        host: TUNNEL_HOST.to_string(),
        web_ui_url: format!("https://{TUNNEL_HOST}:{SUNSHINE_API_PORT}/"),
        sunshine_username: state.credentials.app_username.clone(),
        sunshine_password: state.credentials.app_password.clone(),
        ports: gamestream_ports(),
        tunnel_reachable,
        active_instance_id: state.instance.instance_id,
        configured_executable_path: state.external_moonlight.executable_path.clone(),
        detected_executable_path: detect_default_executable()
            .map(|path| path.display().to_string()),
    })
}

#[tauri::command]
pub async fn external_moonlight_set_executable_path(
    path: Option<String>,
    context: State<'_, AppContext>,
) -> Result<PersistedAppState, FrontendError> {
    let path = path
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if let Some(path) = path.as_deref() {
        if resolve_executable(Path::new(path)).is_none() {
            return Err(AppError::InvalidInput(format!(
                "{path} is not a Moonlight app or executable."
            ))
            .into());
        }
    }

    Ok(context
        .update_state(|state| {
            state.external_moonlight.executable_path = path.clone();
        })
        .await?)
}

/// Approves a PIN shown by any Moonlight client (this computer's or one the user
/// started by hand) that is currently trying to pair with the instance.
#[tauri::command]
pub async fn external_moonlight_submit_pin(
    pin: String,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    let pin = validate_pin(&pin)?;
    ensure_tunnel_reachable().await?;
    approve_pin_until_client_waits(&context, pin, Duration::from_secs(5)).await?;
    Ok(())
}

/// Starts the user's Moonlight in pairing mode with a PIN we choose, then approves that
/// PIN in Sunshine so pairing completes without the user typing anything.
#[tauri::command]
pub async fn external_moonlight_pair(context: State<'_, AppContext>) -> Result<(), FrontendError> {
    let executable = executable_for_state(&context.load_state().await)?;
    ensure_tunnel_reachable().await?;

    let pin = random_pin();
    spawn_detached(&executable, &["pair", TUNNEL_HOST, "--pin", &pin])?;
    approve_pin_until_client_waits(&context, &pin, PAIRING_DEADLINE).await?;
    Ok(())
}

/// Opens the user's Moonlight. With an app name it streams that app directly,
/// otherwise it opens Moonlight's own host list.
#[tauri::command]
pub async fn external_moonlight_launch(
    app_name: Option<String>,
    context: State<'_, AppContext>,
) -> Result<(), FrontendError> {
    let executable = executable_for_state(&context.load_state().await)?;
    let app_name = app_name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    match app_name {
        Some(app_name) => {
            ensure_tunnel_reachable().await?;
            spawn_detached(&executable, &["stream", TUNNEL_HOST, &app_name])?;
        }
        None => spawn_detached(&executable, &[])?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{random_pairing_id, random_pin, resolve_executable, validate_pin};

    #[test]
    fn generated_pin_is_four_digits() {
        for _ in 0..50 {
            let pin = random_pin();
            assert!(validate_pin(&pin).is_ok(), "{pin}");
        }
    }

    #[test]
    fn rejects_malformed_pins() {
        assert!(validate_pin("123").is_err());
        assert!(validate_pin("12a4").is_err());
        assert!(validate_pin("12345").is_err());
        assert_eq!(validate_pin(" 0042 ").unwrap(), "0042");
    }

    #[test]
    fn pairing_id_is_32_hex_chars() {
        let id = random_pairing_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|value| value.is_ascii_hexdigit()));
    }

    #[test]
    fn resolves_app_bundle_to_inner_binary() {
        let root = std::env::temp_dir().join(format!("noland-ext-ml-{}", random_pairing_id()));
        let binary = root.join("Moonlight.app/Contents/MacOS/Moonlight");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"").unwrap();

        assert_eq!(
            resolve_executable(&root.join("Moonlight.app")).as_deref(),
            Some(binary.as_path())
        );
        assert_eq!(
            resolve_executable(&binary).as_deref(),
            Some(binary.as_path())
        );
        assert!(resolve_executable(&root).is_none());
        assert!(resolve_executable(&root.join("missing")).is_none());

        let _ = std::fs::remove_dir_all(root);
    }
}
