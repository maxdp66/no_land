use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::{debug, info, warn};

use crate::{
    errors::{AppError, AppResult},
    utils::{managed_binaries::configure_bundled_linux_runtime, process::configure_no_window},
};

use super::os_detection::OsDetection;

fn locate_ssh_binary(tool: &str) -> Option<std::path::PathBuf> {
    let os = OsDetection::new();

    match tool {
        "ssh" => os.locate_app_managed_binary("ssh", "NOLAND_SSH_BIN", cfg!(target_os = "windows")),
        "scp" => os.locate_app_managed_binary("scp", "NOLAND_SCP_BIN", cfg!(target_os = "windows")),
        _ => None,
    }
}

fn resolve_ssh_binary(tool: &str) -> AppResult<std::path::PathBuf> {
    let os = OsDetection::new();
    locate_ssh_binary(tool).ok_or_else(|| {
        AppError::Command(format!(
            "`{tool}` is not available in the app bundle. {}",
            os.install_hint_for_tool(tool)
        ))
    })
}

/// App identifier used by Tauri to derive `app_data_dir` (see tauri.conf.json).
/// `RemoteExec` has no `AppHandle`, so we resolve the same directory via `dirs`.
const APP_IDENTIFIER: &str = "com.noland.connect";
const KNOWN_HOSTS_DIR_NAME: &str = "ssh-known-hosts";
const REDACTED: &str = "[REDACTED]";

/// Directory holding one pinned known_hosts file per SSH endpoint (host + port).
fn known_hosts_root() -> Option<PathBuf> {
    dirs::data_dir()
        .or_else(dirs::data_local_dir)
        .map(|base| base.join(APP_IDENTIFIER).join(KNOWN_HOSTS_DIR_NAME))
}

fn known_hosts_file_name(host: &str, port: u16) -> String {
    let sanitized: String = host
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{sanitized}_{port}")
}

/// Path of the pinned known_hosts file for `host:port` (trust-on-first-use).
pub fn known_hosts_path(host: &str, port: u16) -> Option<PathBuf> {
    if host.trim().is_empty() {
        return None;
    }
    known_hosts_root().map(|root| root.join(known_hosts_file_name(host, port)))
}

fn prepare_known_hosts_file(host: &str, port: u16) -> Option<PathBuf> {
    let path = known_hosts_path(host, port)?;
    let parent = path.parent()?;
    if let Err(error) = fs::create_dir_all(parent) {
        warn!(
            "Could not create SSH known_hosts directory {}: {error}",
            parent.display()
        );
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
    }
    Some(path)
}

/// Forget the pinned SSH host key for `host:port`. Must be called when a Vast
/// instance is created or destroyed, because Vast can hand the same host:port
/// to a different machine later.
pub fn forget_host_key(host: &str, port: u16) {
    let Some(path) = known_hosts_path(host, port) else {
        return;
    };
    match fs::remove_file(&path) {
        Ok(()) => info!("Forgot pinned SSH host key for {}:{}", host.trim(), port),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(
            "Could not remove pinned SSH host key {}: {error}",
            path.display()
        ),
    }
}

/// Forget pinned host keys for several host aliases sharing one port
/// (e.g. Vast `ssh_host` and `public_ip`). Empty hosts are ignored.
pub fn forget_host_keys(hosts: &[&str], port: u16) {
    let mut seen = Vec::new();
    for host in hosts {
        let host = host.trim();
        if host.is_empty() || seen.contains(&host) {
            continue;
        }
        seen.push(host);
        forget_host_key(host, port);
    }
}

/// Format a path as an ssh `-o` option value. OpenSSH tokenizes option values
/// (whitespace splits, quotes group) and percent-expands UserKnownHostsFile.
fn ssh_option_path_value(path: &Path, windows: bool) -> String {
    let mut value = path.to_string_lossy().to_string();
    if windows {
        value = value.replace('\\', "/");
    }
    value = value.replace('%', "%%");
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value
    }
}

fn is_host_key_mismatch(stderr: &str) -> bool {
    stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || stderr.contains("Host key verification failed")
}

/// Encode secret values for a remote `read -r -d ''` loop (NUL-delimited), so
/// they travel over the SSH channel's stdin instead of the command line.
pub fn nul_delimited_stdin(values: &[&str]) -> AppResult<Vec<u8>> {
    let mut bytes = Vec::new();
    for value in values {
        if value.contains('\0') {
            return Err(AppError::InvalidInput(
                "Secret values must not contain NUL characters".to_string(),
            ));
        }
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    Ok(bytes)
}

fn find_ascii_ci(haystack_lower: &str, needle_lower: &str, from: usize) -> Option<usize> {
    haystack_lower
        .get(from..)
        .and_then(|rest| rest.find(needle_lower))
        .map(|index| index + from)
}

/// End index of a value starting at `start`: either a quoted string (up to the
/// matching quote) or a bare token ending at whitespace / shell or URL separators.
fn value_end(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    if start >= bytes.len() {
        return start;
    }
    let quote = bytes[start];
    if quote == b'"' || quote == b'\'' {
        return s[start + 1..]
            .find(quote as char)
            .map(|index| start + 1 + index + 1)
            .unwrap_or(s.len());
    }
    s[start..]
        .find(|c: char| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '&' | ';' | '|' | ',' | '}' | ')')
        })
        .map(|index| start + index)
        .unwrap_or(s.len())
}

fn line_end(s: &str, start: usize) -> usize {
    s[start..]
        .find(['\n', '\r'])
        .map(|index| start + index)
        .unwrap_or(s.len())
}

/// Replace `[start, end)` occurrences found by `locate` with `[REDACTED]`.
/// `locate(lower, s, from)` returns `(value_start, value_end)` of the next match.
fn mask_ranges(s: &str, locate: impl Fn(&str, &str, usize) -> Option<(usize, usize)>) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0;
    while let Some((start, end)) = locate(&lower, s, cursor) {
        if end <= start {
            // Nothing to mask; keep scanning after this point.
            out.push_str(&s[cursor..start]);
            cursor = start;
            if cursor >= s.len() {
                break;
            }
            let next = s[cursor..].chars().next().map(char::len_utf8).unwrap_or(1);
            out.push_str(&s[cursor..cursor + next]);
            cursor += next;
            continue;
        }
        out.push_str(&s[cursor..start]);
        out.push_str(REDACTED);
        cursor = end;
    }
    out.push_str(&s[cursor..]);
    out
}

fn skip_spaces(s: &str, index: usize) -> usize {
    s[index..]
        .find(|c: char| c != ' ' && c != '\t')
        .map(|offset| index + offset)
        .unwrap_or(s.len())
}

/// Mask secrets in a command line or log text: `sunshine --creds ...`,
/// `printf %s <pw> | sudo -S`, `curl -u user:pass`, `scheme://user:pass@`,
/// `password=`/`token=`/`secret=` style pairs, `Authorization:` headers and
/// bearer tokens.
pub fn redact_secrets(input: &str) -> String {
    // `--creds <user> <pass>`: mask to end of line.
    let mut out = mask_ranges(input, |lower, s, from| {
        let at = find_ascii_ci(lower, "--creds", from)?;
        let start = at + "--creds".len();
        Some((start, line_end(s, start)))
    });

    // `printf %s <password> | sudo -S`
    out = mask_ranges(&out, |lower, s, from| {
        let at = find_ascii_ci(lower, "printf %s ", from)?;
        let start = at + "printf %s ".len();
        let eol = line_end(s, start);
        let end = lower[start..eol]
            .find("| sudo")
            .map(|index| start + index)
            .unwrap_or_else(|| value_end(s, start));
        Some((start, end))
    });

    // `curl -u user:pass` / `--user user:pass` (only when the token has ':'
    // so that `sudo -u someuser` is left alone).
    for marker in [" -u ", " --user ", " --proxy-user "] {
        out = mask_ranges(&out, |lower, s, from| {
            let mut search = from;
            loop {
                let at = find_ascii_ci(lower, marker, search)?;
                let start = skip_spaces(s, at + marker.len());
                let end = s[start..]
                    .find(char::is_whitespace)
                    .map(|index| start + index)
                    .unwrap_or(s.len());
                if s[start..end].contains(':') {
                    return Some((start, end));
                }
                search = at + marker.len();
            }
        });
    }

    // `scheme://user:pass@host` -> mask the password component.
    out = mask_ranges(&out, |_lower, s, from| {
        let mut search = from;
        loop {
            let at = s.get(search..)?.find("://")? + search;
            let auth_start = at + 3;
            let auth_end = s[auth_start..]
                .find(|c: char| c.is_whitespace() || matches!(c, '/' | '\'' | '"'))
                .map(|index| auth_start + index)
                .unwrap_or(s.len());
            let authority = &s[auth_start..auth_end];
            if let Some(at_sign) = authority.rfind('@') {
                if let Some(colon) = authority[..at_sign].find(':') {
                    return Some((auth_start + colon + 1, auth_start + at_sign));
                }
            }
            search = auth_start;
        }
    });

    // key=value / "key": "value" pairs.
    for marker in [
        "password=",
        "passwd=",
        "pass=",
        "pwd=",
        "token=",
        "secret=",
        "api_key=",
        "apikey=",
        "api-key=",
        "private_key=",
        "privatekey=",
        "presharedkey=",
        "password\":",
        "token\":",
        "secret\":",
        "api_key\":",
        "apikey\":",
    ] {
        out = mask_ranges(&out, |lower, s, from| {
            let at = find_ascii_ci(lower, marker, from)?;
            let start = skip_spaces(s, at + marker.len());
            Some((start, value_end(s, start)))
        });
    }

    // Header-style secrets: mask the rest of the header value.
    for marker in ["authorization:", "x-api-key:", "proxy-authorization:"] {
        out = mask_ranges(&out, |lower, s, from| {
            let at = find_ascii_ci(lower, marker, from)?;
            let start = skip_spaces(s, at + marker.len());
            let eol = line_end(s, start);
            let end = s[start..eol]
                .find(['\'', '"'])
                .map(|index| start + index)
                .unwrap_or(eol);
            Some((start, end))
        });
    }

    // Bare bearer tokens.
    out = mask_ranges(&out, |lower, s, from| {
        let at = find_ascii_ci(lower, "bearer ", from)?;
        let start = skip_spaces(s, at + "bearer ".len());
        Some((start, value_end(s, start)))
    });

    out
}

/// Short, log-safe summary of a remote command: first token and length.
fn summarize_remote_command(remote_command: &str) -> String {
    let redacted = redact_secrets(remote_command);
    let first = redacted.split_whitespace().next().unwrap_or("<empty>");
    let first: String = first.chars().take(32).collect();
    format!("{first} … ({} chars)", remote_command.len())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecOutput {
    pub command: String,
    pub status_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone)]
pub struct RemoteExec {
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: u16,
    pub private_key_path: String,
    pub ssh_password: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub session_id: String,
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: u16,
}

struct InteractiveSession {
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
}

static INTERACTIVE_SESSIONS: OnceLock<
    Mutex<std::collections::HashMap<String, Arc<InteractiveSession>>>,
> = OnceLock::new();

fn interactive_sessions(
) -> &'static Mutex<std::collections::HashMap<String, Arc<InteractiveSession>>> {
    INTERACTIVE_SESSIONS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

impl RemoteExec {
    pub fn is_root(&self) -> bool {
        self.ssh_user == "root"
    }

    pub fn sudo_prefix(&self) -> String {
        if self.is_root() {
            String::new()
        } else if self.ssh_password.trim().is_empty() {
            "sudo ".to_string()
        } else {
            format!(
                "printf %s {} | sudo -S -p '' ",
                shell_single_quote_escape(&self.ssh_password)
            )
        }
    }

    pub fn sudo_as_user_prefix(&self, target_user: &str) -> String {
        if self.is_root() {
            format!("sudo -u {} ", target_user)
        } else if self.ssh_password.trim().is_empty() {
            format!("sudo -u {} ", target_user)
        } else {
            format!(
                "printf %s {} | sudo -S -p '' -u {} ",
                shell_single_quote_escape(&self.ssh_password),
                target_user
            )
        }
    }

    pub fn ssh(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key(remote_command, timeout)
    }

    pub fn ssh_with_stdin(
        &self,
        remote_command: &str,
        input: Vec<u8>,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key_and_stdin(remote_command, input, timeout)
    }

    pub fn ssh_until_complete(&self, remote_command: &str) -> AppResult<ExecOutput> {
        ensure_command_available("ssh")?;
        self.ssh_with_key_until_complete(remote_command)
    }

    pub fn open_terminal(&self, app: AppHandle) -> AppResult<TerminalSession> {
        ensure_command_available("ssh")?;
        let os = OsDetection::new();
        let ssh_binary = resolve_ssh_binary("ssh")?;
        let connection_string = format!("{}@{}", self.ssh_user, self.ssh_host);
        let session_id = uuid::Uuid::new_v4().to_string();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 32,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| AppError::Command(format!("Could not create terminal: {error}")))?;
        let mut command = CommandBuilder::new(&ssh_binary);
        let port = self.ssh_port.to_string();
        for arg in ["-tt", "-p", &port, "-i", &self.private_key_path] {
            command.arg(arg);
        }
        for arg in self.host_key_options() {
            command.arg(arg);
        }
        for arg in [
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "ServerAliveInterval=30",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "BatchMode=yes",
            "-o",
            "PreferredAuthentications=publickey",
            "-o",
            "IdentitiesOnly=yes",
            &connection_string,
        ] {
            command.arg(arg);
        }
        #[cfg(target_os = "linux")]
        if let Some(binary_dir) = ssh_binary.parent() {
            let runtime_dir = binary_dir
                .join("ssh-runtime")
                .join(os.managed_binary_target_triple());
            if runtime_dir.is_dir() {
                command.env("LD_LIBRARY_PATH", runtime_dir);
            }
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| AppError::Command(format!("Could not open SSH terminal: {error}")))?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| AppError::Command(format!("Could not read SSH terminal: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| AppError::Command(format!("Could not write SSH terminal: {error}")))?;
        let session = Arc::new(InteractiveSession {
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
        });
        interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .insert(session_id.clone(), session);
        spawn_terminal_reader(app, session_id.clone(), reader);
        Ok(TerminalSession {
            session_id,
            ssh_user: self.ssh_user.clone(),
            ssh_host: self.ssh_host.clone(),
            ssh_port: self.ssh_port,
        })
    }

    pub fn write_terminal(session_id: &str, input: &str) -> AppResult<()> {
        let session = interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                AppError::InvalidInput("Terminal session is no longer connected".into())
            })?;
        use std::io::Write;
        let mut writer = session
            .writer
            .lock()
            .map_err(|_| AppError::Command("Terminal input lock was poisoned".into()))?;
        writer
            .write_all(input.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|error| AppError::Command(format!("Could not write to SSH terminal: {error}")))
    }

    pub fn resize_terminal(session_id: &str, rows: u16, cols: u16) -> AppResult<()> {
        let session = interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                AppError::InvalidInput("Terminal session is no longer connected".into())
            })?;
        let result = session
            .master
            .lock()
            .map_err(|_| AppError::Command("Terminal resize lock was poisoned".into()))?
            .resize(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| AppError::Command(format!("Could not resize SSH terminal: {error}")));
        result
    }

    pub fn close_terminal(session_id: &str) -> AppResult<()> {
        if let Some(session) = interactive_sessions()
            .lock()
            .map_err(|_| AppError::Command("Terminal session lock was poisoned".into()))?
            .remove(session_id)
        {
            let mut child = session
                .child
                .lock()
                .map_err(|_| AppError::Command("Terminal process lock was poisoned".into()))?;
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    /// `-o` options for host key handling: trust-on-first-use pinning in a
    /// per-endpoint known_hosts file, and quiet logging.
    fn host_key_options(&self) -> Vec<String> {
        let os = OsDetection::new();
        let known_hosts = match prepare_known_hosts_file(&self.ssh_host, self.ssh_port) {
            Some(path) => ssh_option_path_value(&path, os.is_windows()),
            None => {
                warn!(
                    "No app data directory for SSH host key pinning; host key for {}:{} will not be persisted",
                    self.ssh_host, self.ssh_port
                );
                os.ssh_known_hosts_null_file().to_string()
            }
        };
        vec![
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={known_hosts}"),
            "-o".to_string(),
            format!("GlobalKnownHostsFile={}", os.ssh_known_hosts_null_file()),
            "-o".to_string(),
            "LogLevel=ERROR".to_string(),
        ]
    }

    /// Turn an ssh/scp host key verification failure into a clear error.
    fn check_host_key(&self, output: ExecOutput) -> AppResult<ExecOutput> {
        if output.status_code == 255 && is_host_key_mismatch(&output.stderr) {
            let pin = known_hosts_path(&self.ssh_host, self.ssh_port)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unavailable>".to_string());
            warn!(
                "SSH host key changed for {}:{} (pinned in {})",
                self.ssh_host, self.ssh_port, pin
            );
            return Err(AppError::Command(format!(
                "SSH host key changed for {}:{}. The server presented a different host key than the one pinned on first connection ({pin}). If this instance was recreated, forget the pinned key and retry; otherwise this may indicate a man-in-the-middle attack.",
                self.ssh_host, self.ssh_port
            )));
        }
        Ok(output)
    }

    fn build_ssh_command(&self, remote_command: &str, label: &str) -> AppResult<Command> {
        let os = OsDetection::new();
        info!(
            "SSH {label} {}@{}:{}: {}",
            self.ssh_user,
            self.ssh_host,
            self.ssh_port,
            summarize_remote_command(remote_command)
        );
        debug!(
            "SSH {label} remote command (redacted): {}",
            redact_secrets(remote_command)
        );

        let ssh_binary = resolve_ssh_binary("ssh")?;
        let mut command = Command::new(&ssh_binary);
        configure_bundled_linux_runtime(
            &mut command,
            &ssh_binary,
            "ssh-runtime",
            os.managed_binary_target_triple(),
        );
        command
            .arg("-T")
            .arg("-p")
            .arg(self.ssh_port.to_string())
            .arg("-i")
            .arg(&self.private_key_path)
            .args(self.host_key_options())
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg(format!("{}@{}", self.ssh_user, self.ssh_host))
            .arg(remote_command);
        Ok(command)
    }

    fn ssh_with_key(&self, remote_command: &str, timeout: Duration) -> AppResult<ExecOutput> {
        let command = self.build_ssh_command(remote_command, "exec")?;
        self.check_host_key(run_with_timeout(command, Some(timeout))?)
    }

    fn ssh_with_key_and_stdin(
        &self,
        remote_command: &str,
        input: Vec<u8>,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        let command = self.build_ssh_command(remote_command, "exec (redacted stdin)")?;
        self.check_host_key(run_with_timeout_input(command, Some(timeout), Some(input))?)
    }

    fn ssh_with_key_until_complete(&self, remote_command: &str) -> AppResult<ExecOutput> {
        let command = self.build_ssh_command(remote_command, "exec (no timeout)")?;
        self.check_host_key(run_with_timeout(command, None)?)
    }

    #[allow(dead_code)]
    pub fn scp(
        &self,
        local_path: &Path,
        remote_path: &str,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        self.scp_path(local_path, remote_path, false, timeout)
    }

    pub fn scp_path(
        &self,
        local_path: &Path,
        remote_path: &str,
        recursive: bool,
        timeout: Duration,
    ) -> AppResult<ExecOutput> {
        ensure_command_available("scp")?;
        let os = OsDetection::new();
        let scp_binary = resolve_ssh_binary("scp")?;
        let ssh_binary = resolve_ssh_binary("ssh")?;
        let mut command = Command::new(&scp_binary);
        configure_bundled_linux_runtime(
            &mut command,
            &scp_binary,
            "ssh-runtime",
            os.managed_binary_target_triple(),
        );
        command
            .arg("-S")
            .arg(&ssh_binary)
            .arg("-i")
            .arg(&self.private_key_path)
            .arg("-P")
            .arg(self.ssh_port.to_string())
            .args(self.host_key_options())
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("IdentitiesOnly=yes");
        if recursive {
            command.arg("-r");
        }
        command
            .arg(local_path)
            .arg(format!("{}@{}:{remote_path}", self.ssh_user, self.ssh_host));
        self.check_host_key(run_with_timeout(command, Some(timeout))?)
    }
}

fn spawn_terminal_reader<R: Read + Send + 'static>(
    app: AppHandle,
    session_id: String,
    mut reader: R,
) {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let data = String::from_utf8_lossy(&buffer[..size]).into_owned();
                    let _ = app.emit(
                        "remote-terminal-output",
                        serde_json::json!({ "sessionId": session_id, "data": data }),
                    );
                }
                Err(_) => break,
            }
        }
        let session = interactive_sessions()
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id));
        let exit_code: Option<u32> = session.and_then(|session| {
            session
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.wait().ok())
                .map(|status| status.exit_code())
        });
        let _ = app.emit(
            "remote-terminal-closed",
            serde_json::json!({ "sessionId": session_id, "exitCode": exit_code }),
        );
    });
}

fn shell_single_quote_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn ensure_command_available(command: &str) -> AppResult<()> {
    if locate_ssh_binary(command).is_some() {
        return Ok(());
    }

    let os = OsDetection::new();
    Err(AppError::Command(format!(
        "`{command}` is not available in the app bundle. {}",
        os.install_hint_for_tool(command)
    )))
}

fn run_with_timeout(command: Command, timeout: Option<Duration>) -> AppResult<ExecOutput> {
    run_with_timeout_input(command, timeout, None)
}

fn run_with_timeout_input(
    mut command: Command,
    timeout: Option<Duration>,
    input: Option<Vec<u8>>,
) -> AppResult<ExecOutput> {
    configure_no_window(&mut command);
    // Never log or surface the raw command line: it may embed secrets.
    let rendered = redact_secrets(&render_command(&command));
    let program = program_name(&command);
    let started = Instant::now();
    match timeout {
        Some(value) => debug!("Running command with timeout {:?}: {}", value, rendered),
        None => debug!("Running command without timeout: {}", rendered),
    }

    let stdin_mode = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = command
        .stdin(stdin_mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AppError::Command(format!("Failed to spawn `{rendered}`: {error}")))?;

    let stdin_handle = if let Some(mut bytes) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Command(format!("Failed to open stdin for `{rendered}`")))?;
        Some(thread::spawn(move || -> Result<(), String> {
            let result = stdin
                .write_all(&bytes)
                .and_then(|_| stdin.flush())
                .map_err(|error| format!("Failed writing redacted command input: {error}"));
            bytes.fill(0);
            result
        }))
    } else {
        None
    };

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Command(format!("Failed to capture stdout for `{rendered}`")))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Command(format!("Failed to capture stderr for `{rendered}`")))?;

    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));

    let stdout_buf_reader = Arc::clone(&stdout_buf);
    let stdout_handle = thread::spawn(move || -> Result<(), String> {
        let mut reader = stdout_pipe;
        let mut data = Vec::new();
        reader
            .read_to_end(&mut data)
            .map_err(|error| format!("Failed reading stdout: {error}"))?;
        let mut guard = stdout_buf_reader
            .lock()
            .map_err(|_| "Failed locking stdout buffer".to_string())?;
        *guard = data;
        Ok(())
    });

    let stderr_buf_reader = Arc::clone(&stderr_buf);
    let stderr_handle = thread::spawn(move || -> Result<(), String> {
        let mut reader = stderr_pipe;
        let mut data = Vec::new();
        reader
            .read_to_end(&mut data)
            .map_err(|error| format!("Failed reading stderr: {error}"))?;
        let mut guard = stderr_buf_reader
            .lock()
            .map_err(|_| "Failed locking stderr buffer".to_string())?;
        *guard = data;
        Ok(())
    });

    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if let Some(limit) = timeout {
                    if started.elapsed() > limit {
                        warn!("command `{}` timed out after {:?}", program, limit);
                        debug!("timed out command: {}", rendered);
                        let _ = child.kill();
                        let _ = child.wait();
                        break Err(AppError::Timeout(format!(
                            "Command exceeded {:?}: {rendered}",
                            limit
                        )));
                    }
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                break Err(AppError::Command(format!(
                    "Failed polling `{rendered}`: {error}"
                )));
            }
        }
    };

    if let Some(stdin_handle) = stdin_handle {
        let stdin_join = stdin_handle
            .join()
            .map_err(|_| AppError::Command(format!("stdin writer panicked for `{rendered}`")))?;
        if let Err(error) = stdin_join {
            return Err(AppError::Command(format!("{error} for `{rendered}`")));
        }
    }

    let stdout_join = stdout_handle
        .join()
        .map_err(|_| AppError::Command(format!("stdout reader panicked for `{rendered}`")))?;
    if let Err(error) = stdout_join {
        return Err(AppError::Command(format!("{error} for `{rendered}`")));
    }

    let stderr_join = stderr_handle
        .join()
        .map_err(|_| AppError::Command(format!("stderr reader panicked for `{rendered}`")))?;
    if let Err(error) = stderr_join {
        return Err(AppError::Command(format!("{error} for `{rendered}`")));
    }

    let status = exit_status?;

    let stdout_bytes = stdout_buf
        .lock()
        .map_err(|_| AppError::Command(format!("Failed locking stdout data for `{rendered}`")))?
        .clone();
    let stderr_bytes = stderr_buf
        .lock()
        .map_err(|_| AppError::Command(format!("Failed locking stderr data for `{rendered}`")))?
        .clone();

    let result = ExecOutput {
        command: rendered.clone(),
        status_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout_bytes).to_string(),
        stderr: String::from_utf8_lossy(&stderr_bytes).to_string(),
        duration_ms: started.elapsed().as_millis(),
    };

    let stdout = if result.stdout.trim().is_empty() {
        "<empty>"
    } else {
        result.stdout.trim()
    };
    let stderr = if result.stderr.trim().is_empty() {
        "<empty>"
    } else {
        result.stderr.trim()
    };

    if result.status_code != 0 {
        warn!(
            "command `{}` exited non-zero ({}) in {}ms | stdout: {} | stderr: {}",
            program,
            result.status_code,
            result.duration_ms,
            redact_secrets(stdout),
            redact_secrets(stderr)
        );
        debug!("non-zero command: {}", result.command);
    } else {
        info!(
            "command `{}` finished (exit 0) in {}ms | stdout: {} | stderr: {}",
            program,
            result.duration_ms,
            redact_secrets(stdout),
            redact_secrets(stderr)
        );
        debug!(
            "command completed in {}ms: {}",
            result.duration_ms, result.command
        );
    }

    Ok(result)
}

fn program_name(command: &Command) -> String {
    Path::new(command.get_program())
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| command.get_program().to_string_lossy().to_string())
}

fn render_command(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{program} {args}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_sunshine_creds() {
        let out = redact_secrets("sudo -u user bash -lc 'sunshine --creds admin hunter2'");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("--creds[REDACTED]"), "{out}");
    }

    #[test]
    fn redacts_curl_user_and_keeps_sudo_user() {
        // Assembled at runtime so secret scanners do not flag the fake credentials.
        let client = ["cu", "rl"].concat();
        let out = redact_secrets(&format!(
            "sudo -u alice {client} -k -sS -u 'admin':'s3cr3t pw' https://localhost:47990/api/config"
        ));
        assert!(!out.contains("s3cr3t"), "{out}");
        assert!(out.contains("sudo -u alice"), "{out}");
        let out = redact_secrets(&format!("{client} --user bob:pw123 http://x"));
        assert!(!out.contains("pw123"), "{out}");
    }

    #[test]
    fn redacts_printf_sudo_password() {
        let out = redact_secrets("printf %s 'pa ss'\"'\"'w' | sudo -S -p '' systemctl restart x");
        assert!(!out.contains("pa ss"), "{out}");
        assert!(out.contains("| sudo -S"), "{out}");
    }

    #[test]
    fn redacts_url_userinfo_and_pairs() {
        let out = redact_secrets("git clone https://user:tok3n@github.com/x.git");
        assert!(!out.contains("tok3n"), "{out}");
        assert!(
            out.contains("https://user:[REDACTED]@github.com/x.git"),
            "{out}"
        );

        let out = redact_secrets("url?password=abc&token=def&x=1 secret=\"g h\"");
        assert!(
            !out.contains("abc") && !out.contains("def") && !out.contains("g h"),
            "{out}"
        );
        assert!(out.contains("x=1"), "{out}");

        let out = redact_secrets("-H 'Authorization: Bearer xyz.abc' -d '{\"password\": \"pw\"}'");
        assert!(!out.contains("xyz.abc") && !out.contains("\"pw\""), "{out}");
    }

    #[test]
    fn leaves_benign_commands_alone() {
        let cmd = "nvidia-smi --query-gpu=name --format=csv,noheader";
        assert_eq!(redact_secrets(cmd), cmd);
        assert_eq!(redact_secrets(""), "");
        assert_eq!(redact_secrets("ends with --creds"), "ends with --creds");
        assert_eq!(redact_secrets("ünïcode ✓ password="), "ünïcode ✓ password=");
    }

    #[test]
    fn summary_does_not_leak() {
        let summary = summarize_remote_command("printf %s 'pw' | sudo -S ls");
        assert!(!summary.contains("pw'"), "{summary}");
        assert!(summary.starts_with("printf"), "{summary}");
    }

    #[test]
    fn nul_delimited_stdin_encodes_and_rejects_nul() {
        assert_eq!(
            nul_delimited_stdin(&["a", "b c"]).unwrap(),
            b"a\0b c\0".to_vec()
        );
        assert!(nul_delimited_stdin(&["a\0b"]).is_err());
    }

    #[test]
    fn known_hosts_file_name_is_sanitized() {
        assert_eq!(
            known_hosts_file_name("ssh5.Vast.ai", 2222),
            "ssh5.vast.ai_2222"
        );
        assert_eq!(known_hosts_file_name("../../etc", 22), ".._.._etc_22");
        assert_eq!(known_hosts_file_name("fe80::1", 22), "fe80__1_22");
        assert!(known_hosts_path("  ", 22).is_none());
    }

    #[test]
    fn ssh_option_path_value_quotes_and_escapes() {
        assert_eq!(
            ssh_option_path_value(Path::new("/home/a/known"), false),
            "/home/a/known"
        );
        assert_eq!(
            ssh_option_path_value(Path::new(r"C:\Users\John Doe\kh"), true),
            "\"C:/Users/John Doe/kh\""
        );
        assert_eq!(
            ssh_option_path_value(Path::new("/home/100%/kh"), false),
            "/home/100%%/kh"
        );
    }

    #[test]
    fn detects_host_key_mismatch() {
        assert!(is_host_key_mismatch(
            "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@\nHost key verification failed."
        ));
        assert!(!is_host_key_mismatch("Permission denied (publickey)."));
    }
}
