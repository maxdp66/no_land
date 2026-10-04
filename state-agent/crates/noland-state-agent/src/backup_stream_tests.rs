//! Regression coverage for uploads that previously staged the entire selection.
use crate::{backup::run_backup, AgentConfig, StateAgent};
use async_trait::async_trait;
use bytes::Bytes;
use noland_crypto::MasterKey;
use noland_state_core::*;
use noland_storage::{
    Health, LocalStorage, RemoteEntry, RemoteKey, RemoteMeta, SharedStorageProvider,
};
use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

struct InspectingStorage<'a> {
    local: LocalStorage,
    agent: &'a StateAgent,
    operation_id: uuid::Uuid,
    fail: AtomicBool,
    uploads: AtomicU64,
    first_upload_files: AtomicU64,
}

#[async_trait]
impl SharedStorageProvider for InspectingStorage<'_> {
    async fn health_check(&self) -> Result<Health> {
        self.local.health_check().await
    }
    async fn ensure_root(&self) -> Result<()> {
        self.local.ensure_root().await
    }
    async fn stat(&self, key: &RemoteKey) -> Result<Option<RemoteMeta>> {
        self.local.stat(key).await
    }
    async fn upload_immutable(&self, path: &Path, key: &RemoteKey) -> Result<RemoteMeta> {
        if key.as_str().starts_with("packs/") {
            assert!(fs::metadata(path)?.len() < 80 * 1024 * 1024);
            assert_eq!(fs::read_dir(&self.agent.config.paths.snapshots)?.count(), 0);
            assert!(!self.agent.config.paths.cache.join("cas/chunks").exists());
            let previous = self.uploads.fetch_add(1, Ordering::SeqCst);
            if previous == 0 {
                let progress = self
                    .agent
                    .db
                    .get_operation_progress(self.operation_id)?
                    .unwrap();
                self.first_upload_files.store(
                    progress.detail_json["files_hashed"].as_u64().unwrap_or(0),
                    Ordering::SeqCst,
                );
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(StateError::Storage("simulated provider failure".into()));
            }
        }
        self.local.upload_immutable(path, key).await
    }
    async fn download(&self, key: &RemoteKey, path: &Path) -> Result<()> {
        self.local.download(key, path).await
    }
    async fn list_prefix(&self, key: &RemoteKey) -> Result<Vec<RemoteEntry>> {
        self.local.list_prefix(key).await
    }
    async fn put_small_versioned(&self, bytes: Bytes, key: &RemoteKey) -> Result<RemoteMeta> {
        self.local.put_small_versioned(bytes, key).await
    }
}

#[tokio::test]
async fn uploads_before_all_files_are_read_and_failed_retries_leave_no_local_packs() {
    let root = std::env::temp_dir().join(format!("bounded-backup-{}", uuid::Uuid::new_v4()));
    let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
    let folder = agent.config.home.join("project");
    fs::create_dir_all(&folder).unwrap();
    // Unique chunks total more than the regular pack target; compressible bytes
    // keep the test fast while still exercising the plaintext staging limit.
    for index in 0..80 {
        fs::write(
            folder.join(format!("{index}.bin")),
            vec![index as u8; 1024 * 1024],
        )
        .unwrap();
    }
    let id = crate::folders::register(&agent, "project").unwrap();
    let storage = InspectingStorage {
        local: LocalStorage::new(root.join("cloud")),
        agent: &agent,
        operation_id: uuid::Uuid::new_v4(),
        fail: AtomicBool::new(true),
        uploads: AtomicU64::new(0),
        first_upload_files: AtomicU64::new(u64::MAX),
    };
    let master = MasterKey::generate();
    let error = run_backup(
        &agent,
        &id,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &master,
        Some(storage.operation_id),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("simulated provider failure"));
    assert!(storage.first_upload_files.load(Ordering::SeqCst) < 80);
    assert_eq!(fs::read_dir(&agent.config.paths.packs).unwrap().count(), 0);
    assert!(storage
        .stat(&RemoteKey::new(noland_state_core::catalog_latest_key()))
        .await
        .unwrap()
        .is_none());
    storage.fail.store(false, Ordering::SeqCst);
    storage.uploads.store(0, Ordering::SeqCst);
    let manifest = run_backup(
        &agent,
        &id,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &master,
        Some(storage.operation_id),
    )
    .await
    .unwrap();
    assert_eq!(
        manifest
            .files
            .iter()
            .filter(|file| file.file_type == "file")
            .count(),
        80
    );
    assert!(storage.uploads.load(Ordering::SeqCst) >= 2);
    assert_eq!(fs::read_dir(&agent.config.paths.packs).unwrap().count(), 0);
    assert_eq!(
        agent
            .db
            .sync_journal_summary(storage.operation_id)
            .unwrap()
            .failed_items,
        0
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_single_large_file_does_not_require_a_full_local_copy() {
    use std::io::Write;
    let root = std::env::temp_dir().join(format!("large-file-backup-{}", uuid::Uuid::new_v4()));
    let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
    let folder = agent.config.home.join("project");
    fs::create_dir_all(&folder).unwrap();
    let mut input = fs::File::create(folder.join("large.bin")).unwrap();
    for index in 0..16 {
        input.write_all(&vec![index; 8 * 1024 * 1024]).unwrap();
    }
    drop(input);
    let id = crate::folders::register(&agent, "project").unwrap();
    let storage = InspectingStorage {
        local: LocalStorage::new(root.join("cloud")),
        agent: &agent,
        operation_id: uuid::Uuid::new_v4(),
        fail: AtomicBool::new(false),
        uploads: AtomicU64::new(0),
        first_upload_files: AtomicU64::new(u64::MAX),
    };
    let manifest = run_backup(
        &agent,
        &id,
        BackupMode::CompleteApplication,
        BackupPerformanceMode::Balanced,
        &storage,
        &MasterKey::generate(),
        Some(storage.operation_id),
    )
    .await
    .unwrap();
    assert_eq!(manifest.logical_size(), 128 * 1024 * 1024);
    assert_eq!(storage.first_upload_files.load(Ordering::SeqCst), 0);
    assert!(storage.uploads.load(Ordering::SeqCst) >= 2);
    assert_eq!(fs::read_dir(&agent.config.paths.packs).unwrap().count(), 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restart_releases_obsolete_backup_staging() {
    let root = std::env::temp_dir().join(format!("backup-recovery-{}", uuid::Uuid::new_v4()));
    let agent = StateAgent::boot(AgentConfig::isolated(root.clone())).unwrap();
    for staging in [&agent.config.paths.packs, &agent.config.paths.snapshots] {
        fs::create_dir_all(staging.join("interrupted")).unwrap();
        fs::write(staging.join("interrupted/payload"), b"obsolete staging").unwrap();
    }
    fs::create_dir_all(&agent.config.home).unwrap();
    fs::write(agent.config.home.join("source"), b"original data").unwrap();
    agent.recover().unwrap();
    assert_eq!(fs::read_dir(&agent.config.paths.packs).unwrap().count(), 0);
    assert_eq!(
        fs::read_dir(&agent.config.paths.snapshots).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read(agent.config.home.join("source")).unwrap(),
        b"original data"
    );
    fs::remove_dir_all(root).unwrap();
}
