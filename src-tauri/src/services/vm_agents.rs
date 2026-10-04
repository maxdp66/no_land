//! Prebuilt Linux binaries for the agents that run on the remote VM.
//!
//! Release builds bundle the state, lifecycle, network and microphone agents
//! (plus the observer's eBPF object) compiled once in CI, so provisioning no
//! longer installs Rust and compiles them on every new instance. The files are
//! added to each agent's upload under `prebuilt/`; the remote installers use
//! them when the VM can run them and fall back to building from source
//! otherwise (development builds ship no prebuilt set).

use std::{
    collections::HashMap,
    env,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::errors::{AppError, AppResult};

/// Target the bundled binaries are built for. Vast GPU VMs are x86_64; the
/// remote installers check `uname -m` before using them.
pub const PREBUILT_TARGET: &str = "x86_64-unknown-linux-gnu";
const MANIFEST_NAME: &str = "SHA256SUMS";

pub const STATE_AGENT: &str = "noland-state-agent";
pub const LIFECYCLE_AGENT: &str = "noland-lifecycle-agent";
pub const OBSERVER_BPF: &str = "noland_observer.bpf.o";
pub const NETWORK_AGENT: &str = "noland-network-agent";
pub const MIC_RECEIVER: &str = "noland-mic-receiver";

struct PrebuiltSet {
    files: HashMap<String, PathBuf>,
}

fn prebuilt_set() -> Option<&'static PrebuiltSet> {
    static SET: OnceLock<Option<PrebuiltSet>> = OnceLock::new();
    SET.get_or_init(load_prebuilt_set).as_ref()
}

fn load_prebuilt_set() -> Option<PrebuiltSet> {
    let dir = find_prebuilt_dir()?;
    match verify_manifest(&dir) {
        Ok(files) => {
            info!(
                dir = %dir.display(),
                files = files.len(),
                "Using bundled prebuilt VM agent binaries"
            );
            Some(PrebuiltSet { files })
        }
        Err(error) => {
            warn!(
                dir = %dir.display(),
                %error,
                "Bundled VM agent binaries failed verification; agents will be built on the VM"
            );
            None
        }
    }
}

/// Verified path of a bundled prebuilt file, or `None` when this build ships
/// no prebuilt set (the remote installer then builds from source).
pub fn prebuilt_file(name: &str) -> Option<&'static Path> {
    prebuilt_set()?.files.get(name).map(PathBuf::as_path)
}

/// Adds the named prebuilt files to `archive` under `<prefix>/prebuilt/`.
/// Returns false, adding nothing, unless every named file is available.
pub fn append_prebuilt<W: Write>(
    archive: &mut tar::Builder<W>,
    prefix: &Path,
    names: &[&str],
) -> AppResult<bool> {
    let Some(paths) = names
        .iter()
        .map(|name| prebuilt_file(name).map(|path| (*name, path)))
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(false);
    };

    for (name, path) in paths {
        let bytes = fs::read(path).map_err(|error| {
            AppError::Provisioning(format!("Could not read prebuilt {name}: {error}"))
        })?;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(if name.ends_with(".o") { 0o644 } else { 0o755 });
        header.set_mtime(0);
        header.set_cksum();
        archive
            .append_data(
                &mut header,
                prefix.join("prebuilt").join(name),
                bytes.as_slice(),
            )
            .map_err(|error| {
                AppError::Provisioning(format!("Could not archive prebuilt {name}: {error}"))
            })?;
    }
    Ok(true)
}

fn verify_manifest(dir: &Path) -> AppResult<HashMap<String, PathBuf>> {
    let manifest = fs::read_to_string(dir.join(MANIFEST_NAME))
        .map_err(|error| AppError::Provisioning(format!("read {MANIFEST_NAME}: {error}")))?;
    let mut files = HashMap::new();
    for line in manifest.lines().filter(|line| !line.trim().is_empty()) {
        let (expected, name) = parse_manifest_line(line).ok_or_else(|| {
            AppError::Provisioning(format!("malformed {MANIFEST_NAME} line: {line}"))
        })?;
        let path = dir.join(name);
        let actual = file_sha256(&path)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(AppError::Provisioning(format!(
                "checksum mismatch for {name}"
            )));
        }
        files.insert(name.to_string(), path);
    }
    if files.is_empty() {
        return Err(AppError::Provisioning(format!(
            "{MANIFEST_NAME} lists no files"
        )));
    }
    Ok(files)
}

/// Parses a `sha256sum` line: `<64 hex>  <name>` (text or `*` binary mode).
fn parse_manifest_line(line: &str) -> Option<(&str, &str)> {
    let (hash, rest) = line.split_once(char::is_whitespace)?;
    let name = rest.trim_start().trim_start_matches('*').trim_end();
    let valid_hash = hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit());
    let valid_name = !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != "..";
    (valid_hash && valid_name).then_some((hash, name))
}

fn file_sha256(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)
        .map_err(|error| AppError::Provisioning(format!("open {}: {error}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| AppError::Provisioning(format!("read {}: {error}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn find_prebuilt_dir() -> Option<PathBuf> {
    if let Ok(explicit) = env::var("NOLAND_VM_AGENTS_DIR") {
        let candidate = PathBuf::from(explicit.trim());
        if candidate.join(MANIFEST_NAME).is_file() {
            return Some(candidate);
        }
    }

    let relative_candidates = [
        "vm-agents",
        "resources/vm-agents",
        "Resources/vm-agents",
        "../Resources/vm-agents",
        "../resources/vm-agents",
        "usr/lib/noland-connect/resources/vm-agents",
        "usr/lib/noland-connect/vm-agents",
        "lib/noland-connect/resources/vm-agents",
        "lib/noland-connect/vm-agents",
    ];
    let mut seeds = Vec::new();
    if let Ok(resource_dir) = env::var("NOLAND_TAURI_RESOURCE_DIR") {
        let resource_dir = PathBuf::from(resource_dir.trim());
        if !resource_dir.as_os_str().is_empty() {
            seeds.push(resource_dir);
        }
    }
    if let Ok(current_exe) = env::current_exe() {
        seeds.extend(
            current_exe
                .ancestors()
                .skip(1)
                .take(6)
                .map(Path::to_path_buf),
        );
    }
    seeds.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    seeds.iter().find_map(|seed| {
        relative_candidates.iter().find_map(|relative| {
            let candidate = seed.join(relative).join(PREBUILT_TARGET);
            candidate.join(MANIFEST_NAME).is_file().then_some(candidate)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_and_binary_mode_manifest_lines() {
        let hash = "a".repeat(64);
        assert_eq!(
            parse_manifest_line(&format!("{hash}  noland-state-agent")),
            Some((hash.as_str(), "noland-state-agent"))
        );
        assert_eq!(
            parse_manifest_line(&format!("{hash} *noland_observer.bpf.o")),
            Some((hash.as_str(), "noland_observer.bpf.o"))
        );
    }

    #[test]
    fn rejects_bad_hashes_and_paths() {
        let hash = "b".repeat(64);
        assert_eq!(parse_manifest_line("abc  noland-state-agent"), None);
        assert_eq!(parse_manifest_line(&format!("{hash}  ../evil")), None);
        assert_eq!(parse_manifest_line(&format!("{hash}  dir/file")), None);
        assert_eq!(parse_manifest_line(&format!("{hash}  ..")), None);
    }

    #[test]
    fn verify_manifest_detects_tampering() {
        let dir = env::temp_dir().join(format!("noland-vm-agents-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("noland-network-agent"), b"binary").unwrap();
        let good = hex::encode(Sha256::digest(b"binary"));
        fs::write(
            dir.join(MANIFEST_NAME),
            format!("{good}  noland-network-agent\n"),
        )
        .unwrap();
        assert!(verify_manifest(&dir).is_ok());

        fs::write(dir.join("noland-network-agent"), b"tampered").unwrap();
        assert!(verify_manifest(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
