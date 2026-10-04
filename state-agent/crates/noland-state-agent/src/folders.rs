//! Explicit folder backups use home-relative paths so they restore on a new VM.
use crate::StateAgent;
use chrono::Utc;
use noland_state_core::*;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn register(agent: &StateAgent, input: &str) -> Result<AppId> {
    let home = fs::canonicalize(&agent.config.home)?;
    let path = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        home.join(input)
    };
    let folder = fs::canonicalize(&path)?;
    let relative = folder.strip_prefix(&home).map_err(|_| {
        StateError::UnsafePath("Choose a folder inside the VM user's home directory".into())
    })?;
    if !folder.is_dir()
        || relative.as_os_str().is_empty()
        || agent.config.paths.is_internal(&folder)
        || agent.config.paths.state_root.starts_with(&folder)
        || agent.config.paths.run_root.starts_with(&folder)
    {
        return Err(StateError::UnsafePath(
            "Choose a subfolder outside the worker's storage".into(),
        ));
    }
    let relative = relative
        .to_str()
        .ok_or_else(|| StateError::Invalid("Folder path must be UTF-8".into()))?;
    let id = AppId(format!(
        "folder:{}",
        blake3::hash(relative.as_bytes()).to_hex()
    ));
    agent
        .db
        .upsert_app(&AppIdentity::new(id.clone(), format!("Folder: {relative}")))?;
    agent
        .db
        .add_known_root(&id, "folder", &folder.to_string_lossy())?;
    Ok(id)
}

pub(crate) fn restore_root(
    agent: &StateAgent,
    manifest: &BundleManifest,
) -> Result<Option<PathBuf>> {
    if !manifest.app.app_id.as_str().starts_with("folder:") {
        return Ok(None);
    }
    let relative = manifest
        .environment
        .get("folder_relative_path")
        .and_then(|value| value.as_str())
        .ok_or_else(|| StateError::Invalid("Folder bundle is missing its root path".into()))?;
    validate_relative_path(relative)?;
    let expected_id = format!("folder:{}", blake3::hash(relative.as_bytes()).to_hex());
    let folder = agent.config.home.join(relative);
    if relative.is_empty()
        || manifest.app.app_id.as_str() != expected_id
        || agent.config.paths.is_internal(&folder)
        || agent.config.paths.state_root.starts_with(&folder)
        || agent.config.paths.run_root.starts_with(&folder)
        || manifest.files.iter().any(|file| {
            file.logical_root != "$HOME" || !Path::new(&file.relative_path).starts_with(relative)
        })
    {
        return Err(StateError::UnsafePath(
            "Invalid folder backup root or contents".into(),
        ));
    }
    Ok(Some(folder))
}

pub fn scan(
    agent: &StateAgent,
    app_id: &AppId,
) -> Result<(Vec<(PathRecord, PathAssociation)>, Vec<ManifestFile>)> {
    let root = agent
        .db
        .known_roots(Some(app_id))?
        .into_iter()
        .find(|(_, kind, _)| kind == "folder")
        .map(|(_, _, path)| PathBuf::from(path))
        .ok_or_else(|| {
            StateError::NotFound("Folder backup root is missing; select the folder again".into())
        })?;
    let home = fs::canonicalize(&agent.config.home)?;
    let canonical = fs::canonicalize(&root)?;
    if canonical != root || !root.starts_with(&home) || agent.config.paths.is_internal(&root) {
        return Err(StateError::UnsafePath(
            "Folder backup root changed or is outside home".into(),
        ));
    }
    let mut pending = vec![root.clone()];
    let mut files = Vec::new();
    let mut metadata_entries = Vec::new();
    while let Some(path) = pending.pop() {
        if agent.config.paths.is_internal(&path) {
            continue;
        }
        let meta = fs::symlink_metadata(&path)?;
        let relative = path
            .strip_prefix(&home)
            .map_err(|_| StateError::UnsafePath(path.display().to_string()))?
            .to_str()
            .ok_or_else(|| StateError::Invalid("Folder contains a non-UTF-8 path".into()))?
            .to_string();
        let kind = if meta.is_dir() {
            "directory"
        } else if meta.file_type().is_symlink() {
            "symlink"
        } else if meta.is_file() {
            "file"
        } else {
            return Err(StateError::Invalid(format!(
                "Unsupported special file: {}",
                path.display()
            )));
        };
        let now = Utc::now();
        let record = PathRecord {
            path_id: agent.db.upsert_path(&path.to_string_lossy())?,
            canonical_path: path.to_string_lossy().into_owned(),
            logical_root: Some("$HOME".into()),
            relative_path: Some(relative.clone()),
            file_type: Some(kind.into()),
            mount_id: None,
            size: Some(meta.len() as i64),
            mtime_ns: meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|time| time.as_nanos().min(i64::MAX as u128) as i64),
            #[cfg(unix)]
            inode: Some(meta.ino() as i64),
            #[cfg(not(unix))]
            inode: None,
            #[cfg(unix)]
            mode: Some(meta.mode() as i64),
            #[cfg(not(unix))]
            mode: None,
            #[cfg(unix)]
            uid: Some(meta.uid() as i64),
            #[cfg(not(unix))]
            uid: None,
            #[cfg(unix)]
            gid: Some(meta.gid() as i64),
            #[cfg(not(unix))]
            gid: None,
            content_hash: None,
            last_scanned_at: Some(now.timestamp()),
        };
        let association = PathAssociation {
            app_id: app_id.clone(),
            path_id: record.path_id,
            confidence: 1.0,
            evidence: vec![],
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::UserState,
            first_seen_at: now,
            last_seen_at: now,
        };
        if kind == "file" {
            files.push((record, association));
            continue;
        }
        let target = if kind == "symlink" {
            let target = fs::read_link(&path)?;
            if target.is_absolute() || target.to_string_lossy().contains("..") {
                return Err(StateError::UnsafePath(format!(
                    "Symlink target cannot be restored: {}",
                    path.display()
                )));
            }
            // Never follow links while scanning. The checks above match the
            // existing restore transaction's supported symlink policy.
            Some(
                target
                    .to_str()
                    .ok_or_else(|| StateError::Invalid("Symlink target must be UTF-8".into()))?
                    .into(),
            )
        } else {
            None
        };
        metadata_entries.push(ManifestFile {
            logical_root: "$HOME".into(),
            relative_path: relative,
            source_path_hint: Some(record.canonical_path),
            file_type: kind.into(),
            size: 0,
            file_hash: noland_cas::blake3_hex(b""),
            chunks: vec![],
            mode: record.mode.map(|v| v as u32),
            mtime_ns: record.mtime_ns,
            uid: record.uid.map(|v| v as u32),
            gid: record.gid.map(|v| v as u32),
            symlink_target: target,
            persistence_class: PersistenceClass::PersistentState,
            semantic_role: SemanticRole::UserState,
            association_confidence: 1.0,
            shared_app_ids: vec![],
        });
        if kind == "directory" {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok((files, metadata_entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{backup::run_backup_to_local, AgentConfig};
    use noland_crypto::MasterKey;
    use noland_restore::{download_and_verify, prepare_restore, RestoreTarget, RestoreTransaction};
    use noland_storage::{read_pack_index, LocalStorage};

    #[tokio::test]
    async fn folder_round_trip_preserves_nested_files_empty_directories_and_links() {
        let root = std::env::temp_dir().join(format!("folder-roundtrip-{}", uuid::Uuid::new_v4()));
        let agent = StateAgent::boot(AgentConfig::isolated(root.join("source"))).unwrap();
        let folder = agent.config.home.join("Documents/My Project");
        fs::create_dir_all(folder.join("empty/nested")).unwrap();
        fs::create_dir_all(folder.join("content")).unwrap();
        fs::write(folder.join("content/save.dat"), b"my saved project").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("content/save.dat", folder.join("latest")).unwrap();
        let id = register(&agent, "Documents/My Project").unwrap();
        let master = MasterKey::generate();
        let cloud = root.join("cloud");
        let manifest = run_backup_to_local(
            &agent,
            &id,
            BackupMode::CompleteApplication,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        assert!(manifest
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("empty/nested")
                && file.file_type == "directory"));
        assert_eq!(fs::read_dir(&agent.config.paths.packs).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(&agent.config.paths.snapshots).unwrap().count(),
            0
        );
        assert!(!agent.config.paths.cache.join("cas/chunks").exists());
        let destination =
            StateAgent::boot(AgentConfig::isolated(root.join("destination"))).unwrap();
        fs::create_dir_all(&destination.config.home).unwrap();
        let storage = LocalStorage::new(&cloud);
        let plan = prepare_restore(
            &storage,
            &master,
            &destination.config.paths,
            &id,
            manifest.bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap();
        let index = read_pack_index(&storage, &master, &id, manifest.bundle_id)
            .await
            .unwrap();
        download_and_verify(&storage, &master, &plan, &index)
            .await
            .unwrap();
        destination
            .db
            .upsert_app(&AppIdentity::new(id.clone(), "Restored folder"))
            .unwrap();
        let restored_root = restore_root(&destination, &manifest).unwrap().unwrap();
        let roots = LogicalRootMap::from_home(&destination.config.home);
        let mut transaction = RestoreTransaction::new(&plan, &roots, Some(&destination.db));
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        let verification = transaction.commit_verified().unwrap();
        assert_eq!(verification.files_verified, 1);
        assert_eq!(verification.directories_verified, 4);
        assert_eq!(
            verification.bytes_verified,
            b"my saved project".len() as u64
        );
        #[cfg(unix)]
        assert_eq!(verification.symlinks_verified, 1);
        destination
            .db
            .add_known_root(&id, "folder", &restored_root.to_string_lossy())
            .unwrap();
        assert!(scan(&destination, &id).is_ok());
        let restored = destination.config.home.join("Documents/My Project");
        assert_eq!(
            fs::read(restored.join("content/save.dat")).unwrap(),
            b"my saved project"
        );
        assert!(restored.join("empty/nested").is_dir());
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(restored.join("latest")).unwrap(),
            PathBuf::from("content/save.dat")
        );
        // A later save is a fresh folder snapshot and does not resurrect deleted files.
        fs::remove_file(folder.join("content/save.dat")).unwrap();
        let second =
            run_backup_to_local(&agent, &id, BackupMode::CompleteApplication, cloud, &master)
                .await
                .unwrap();
        assert!(!second
            .files
            .iter()
            .any(|file| file.relative_path.ends_with("content/save.dat")));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn corrupted_cloud_pack_preserves_existing_folder_and_retry_verifies_restored_contents() {
        let root = std::env::temp_dir().join(format!("folder-corruption-{}", uuid::Uuid::new_v4()));
        let source = StateAgent::boot(AgentConfig::isolated(root.join("source"))).unwrap();
        let folder = source.config.home.join("project");
        fs::create_dir_all(folder.join("empty/nested")).unwrap();
        fs::write(folder.join("data.bin"), b"content").unwrap();
        fs::write(folder.join("empty.txt"), b"").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("data.bin", folder.join("current")).unwrap();
        let id = register(&source, "project").unwrap();
        let master = MasterKey::generate();
        let cloud = root.join("cloud");
        let manifest = run_backup_to_local(
            &source,
            &id,
            BackupMode::CompleteApplication,
            cloud.clone(),
            &master,
        )
        .await
        .unwrap();
        let storage = LocalStorage::new(&cloud);
        let index = read_pack_index(&storage, &master, &id, manifest.bundle_id)
            .await
            .unwrap();
        let remote_pack = cloud.join(noland_state_core::pack_key(&index[0].pack_id));
        let original_pack = fs::read(&remote_pack).unwrap();
        let mut corrupted = original_pack.clone();
        *corrupted.last_mut().unwrap() ^= 0x80;
        fs::write(&remote_pack, corrupted).unwrap();
        let destination =
            StateAgent::boot(AgentConfig::isolated(root.join("destination"))).unwrap();
        let restored = destination.config.home.join("project");
        fs::create_dir_all(&restored).unwrap();
        fs::write(restored.join("data.bin"), b"previous version").unwrap();
        fs::write(restored.join("unrelated.txt"), b"keep me").unwrap();
        let plan = prepare_restore(
            &storage,
            &master,
            &destination.config.paths,
            &id,
            manifest.bundle_id,
            RestoreMode::CompleteApplication,
        )
        .await
        .unwrap();
        assert!(download_and_verify(&storage, &master, &plan, &index)
            .await
            .is_err());
        assert_eq!(
            fs::read(restored.join("data.bin")).unwrap(),
            b"previous version"
        );
        assert!(!restored.join("empty.txt").exists());
        // Retry the same staging workspace: the invalid cache must be replaced.
        fs::write(&remote_pack, original_pack).unwrap();
        download_and_verify(&storage, &master, &plan, &index)
            .await
            .unwrap();
        destination
            .db
            .upsert_app(&AppIdentity::new(id, "Folder: project"))
            .unwrap();
        let roots = LogicalRootMap::from_home(&destination.config.home);
        let mut transaction = RestoreTransaction::new(&plan, &roots, Some(&destination.db));
        transaction.publish_to(RestoreTarget::Complete).unwrap();
        let report = transaction.commit_verified().unwrap();
        assert_eq!(report.files_verified, 2);
        assert_eq!(report.directories_verified, 3);
        assert_eq!(report.bytes_verified, 7);
        #[cfg(unix)]
        assert_eq!(report.symlinks_verified, 1);
        assert_eq!(fs::read(restored.join("data.bin")).unwrap(), b"content");
        assert_eq!(fs::metadata(restored.join("empty.txt")).unwrap().len(), 0);
        assert!(restored.join("empty/nested").is_dir());
        assert_eq!(
            fs::read(restored.join("unrelated.txt")).unwrap(),
            b"keep me"
        );
        assert!(!plan.staging.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_outside_home_internal_storage_and_escaping_links() {
        let root = std::env::temp_dir().join(format!("folder-safety-{}", uuid::Uuid::new_v4()));
        let mut config = AgentConfig::isolated(root.clone());
        config.paths = AgentPaths::from_roots(root.join("home/worker"), root.join("run"));
        let agent = StateAgent::boot(config).unwrap();
        fs::create_dir_all(agent.config.home.join("project")).unwrap();
        assert!(register(&agent, "..").is_err());
        assert!(register(&agent, ".").is_err());
        assert!(register(&agent, &agent.config.paths.state_root.to_string_lossy()).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("../../outside", agent.config.home.join("project/escape"))
                .unwrap();
            let id = register(&agent, "project").unwrap();
            assert!(scan(&agent, &id).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
