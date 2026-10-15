use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use fiber_lsp_sdk::{MemoryStore, SignerStore};
use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::Mutex;

pub const ACCOUNT_FILES: [&str; 3] = ["root-key", "signer.bin", "account.json"];

pub struct AccountDir {
    pub path: PathBuf,
    _lock: File,
}

impl AccountDir {
    pub fn lock(path: &Path) -> Result<Self> {
        let parent = path.parent().context("账户目录缺少父目录")?;
        private_dir(parent)?;
        private_dir(path)?;
        let lock_path = path.join(".lock");
        if lock_path.try_exists()? {
            check_private_file(&lock_path)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .with_context(|| format!("无法打开账户锁 {}", lock_path.display()))?;
        lock.try_lock()
            .context("账户正在被其他进程使用，或无法锁定；请稍后重试")?;
        Ok(Self {
            path: path.to_path_buf(),
            _lock: lock,
        })
    }

    pub fn is_initialized(&self) -> Result<bool> {
        let present: Vec<bool> = ACCOUNT_FILES
            .iter()
            .map(|name| self.path.join(name).try_exists())
            .collect::<std::io::Result<_>>()?;
        if present.iter().all(|value| *value) {
            return Ok(true);
        }
        let has_data = fs::read_dir(&self.path)?
            .collect::<std::io::Result<Vec<_>>>()?
            .iter()
            .any(|entry| entry.file_name() != ".lock");
        if has_data {
            let missing = ACCOUNT_FILES
                .iter()
                .zip(present)
                .filter_map(|(name, exists)| (!exists).then_some(*name))
                .collect::<Vec<_>>()
                .join("、");
            bail!(
                "{} 的账户初始化不完整，缺少 {missing}；原文件已保留，不能覆盖或重新生成身份",
                self.path.display()
            );
        }
        Ok(false)
    }

    pub fn read_json<T: DeserializeOwned>(&self) -> Result<T> {
        let path = self.path.join("account.json");
        let bytes = read_private(&path)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("{} 损坏或格式不支持；原文件已保留", path.display()))
    }

    pub fn save_json<T: Serialize>(&self, value: &T) -> Result<()> {
        let _timing = crate::timings::Span::new("store.account.serialize");
        let bytes = serde_json::to_vec_pretty(value).context("无法编码账户记录")?;
        drop(_timing);
        atomic_write(&self.path.join("account.json"), &bytes)
    }
}

fn private_dir(path: &Path) -> Result<()> {
    if path.try_exists()? {
        if !fs::symlink_metadata(path)?.file_type().is_dir() {
            bail!("{} 必须是普通目录；原文件已保留", path.display());
        }
    } else {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .with_context(|| format!("无法创建账户目录 {}", path.display()))?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    File::open(path)?.sync_all()?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn check_private_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("无法读取账户文件 {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o077 != 0 {
        bail!(
            "{} 必须是仅当前用户可访问的普通文件（权限 600）；原文件已保留",
            path.display()
        );
    }
    Ok(())
}

pub fn read_private(path: &Path) -> Result<Vec<u8>> {
    check_private_file(path)?;
    fs::read(path).with_context(|| format!("无法读取 {}；原文件已保留", path.display()))
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let _write = crate::timings::Span::new(match path.file_name().and_then(|n| n.to_str()) {
        Some("signer.bin") => "store.signer.write",
        Some("payments.json") => "store.payments.write",
        Some("account.json") => "store.account.write",
        _ => "store.other.write",
    });
    let parent = path.parent().context("账户文件缺少父目录")?;
    if path.try_exists()? {
        check_private_file(path)?;
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("无法创建 {} 的临时文件", path.display()))?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(bytes)
        .with_context(|| format!("无法写入 {}", path.display()))?;
    let file_sync = crate::timings::Span::new("store.file_fsync");
    temp.as_file()
        .sync_all()
        .with_context(|| format!("无法同步 {}", path.display()))?;
    drop(file_sync);
    let rename = crate::timings::Span::new("store.rename");
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("无法原子替换 {}", path.display()))?;
    drop(rename);
    let _dir_sync = crate::timings::Span::new("store.directory_fsync");
    File::open(parent)?
        .sync_all()
        .with_context(|| format!("无法同步 {} 的目录", path.display()))?;
    Ok(())
}

/// Publish a complete, durable private file without replacing any existing entry.
pub fn create_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("输出文件缺少父目录")?;
    fs::create_dir_all(parent).with_context(|| format!("无法创建输出目录 {}", parent.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path).map_err(|error| error.error)
        .with_context(|| format!("无法首次创建 {}；目标可能已存在（含符号链接），原文件已保留；请复用凭证或指定其他 --output", path.display()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[derive(Clone)]
pub struct FileSignerStore {
    state: Arc<Mutex<StoreState>>,
    path: PathBuf,
}

struct StoreState {
    memory: MemoryStore,
    write_failed: bool,
}

enum Change<'a> {
    Insert(&'a [u8]),
    Cas(Option<&'a [u8]>, &'a [u8]),
    Put(&'a [u8]),
    Delete,
}

impl FileSignerStore {
    pub fn new(dir: &AccountDir) -> Self {
        Self::with_memory(dir, MemoryStore::default())
    }

    pub fn open(dir: &AccountDir) -> Result<Self> {
        let path = dir.path.join("signer.bin");
        let bytes = read_private(&path)?;
        let memory = MemoryStore::from_snapshot(&bytes)
            .with_context(|| format!("{} 的 SDK 快照损坏；原文件已保留", path.display()))?;
        Ok(Self::with_memory(dir, memory))
    }

    fn with_memory(dir: &AccountDir, memory: MemoryStore) -> Self {
        Self {
            state: Arc::new(Mutex::new(StoreState {
                memory,
                write_failed: false,
            })),
            path: dir.path.join("signer.bin"),
        }
    }

    async fn change(&self, key: &[u8], change: Change<'_>) -> Result<bool> {
        let mut state = self.state.lock().await;
        if state.write_failed {
            bail!("SDK 存储曾写入失败；请退出后重新核对账户文件");
        }
        // Mutate a separate SDK snapshot. Publish it in memory only after durable I/O.
        let copy = crate::timings::Span::new("store.signer.snapshot_copy");
        let candidate = MemoryStore::from_snapshot(&state.memory.snapshot()?)?;
        drop(copy);
        let changed = match change {
            Change::Insert(value) => candidate.insert_if_absent(key, value).await?,
            Change::Cas(expected, value) => {
                candidate.compare_and_swap(key, expected, value).await?
            }
            Change::Put(value) => {
                if candidate.get(key).await?.as_deref() == Some(value) {
                    return Ok(false);
                }
                candidate.put(key, value).await?;
                true
            }
            Change::Delete => {
                if candidate.get(key).await?.is_none() {
                    return Ok(false);
                }
                candidate.delete(key).await?;
                true
            }
        };
        if changed {
            let serialize = crate::timings::Span::new("store.signer.serialize");
            let snapshot = candidate.snapshot()?;
            drop(serialize);
            if let Err(error) = atomic_write(&self.path, &snapshot) {
                state.write_failed = true;
                return Err(error);
            }
            state.memory = candidate;
        }
        Ok(changed)
    }
}

#[async_trait::async_trait]
impl SignerStore for FileSignerStore {
    type Error = anyhow::Error;

    async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let state = self.state.lock().await;
        if state.write_failed {
            bail!("SDK 存储曾写入失败；请退出后重新核对账户文件");
        }
        Ok(state.memory.get(key).await?)
    }

    async fn insert_if_absent(&self, key: &[u8], value: &[u8]) -> Result<bool> {
        self.change(key, Change::Insert(value)).await
    }

    async fn compare_and_swap(
        &self,
        key: &[u8],
        expected: Option<&[u8]>,
        replacement: &[u8],
    ) -> Result<bool> {
        self.change(key, Change::Cas(expected, replacement)).await
    }

    async fn put(&self, key: &[u8], value: &[u8]) -> Result<()> {
        self.change(key, Change::Put(value)).await?;
        Ok(())
    }

    async fn delete(&self, key: &[u8]) -> Result<()> {
        self.change(key, Change::Delete).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fiber_lsp_sdk::{RootKey, RootSigner};

    #[tokio::test]
    async fn identity_and_store_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
        let store = FileSignerStore::new(&dir);
        let created = RootSigner::create_random(store.clone()).await.unwrap();
        let identity = created.root_signer.identity_public_key();
        let backup = created.root_key_backup.expose_secret();
        store.put(b"key", b"value").await.unwrap();
        let reopened = FileSignerStore::open(&dir).unwrap();
        assert_eq!(reopened.get(b"key").await.unwrap(), Some(b"value".to_vec()));
        let root = RootSigner::open(RootKey::import(backup).unwrap(), reopened.clone())
            .await
            .unwrap();
        assert_eq!(root.identity_public_key(), identity);
        assert!(
            RootSigner::open(RootKey::import([1; 32]).unwrap(), reopened)
                .await
                .is_err()
        );
        assert_eq!(
            fs::metadata(&dir.path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.path.join("signer.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn conditional_writes_do_not_overwrite_and_only_one_contender_wins() {
        let temp = tempfile::tempdir().unwrap();
        let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
        let store = FileSignerStore::new(&dir);
        assert!(store.insert_if_absent(b"key", b"first").await.unwrap());
        assert!(!store.insert_if_absent(b"key", b"other").await.unwrap());
        assert!(
            !store
                .compare_and_swap(b"key", None, b"other")
                .await
                .unwrap()
        );
        assert!(
            !store
                .compare_and_swap(b"key", Some(b"wrong"), b"other")
                .await
                .unwrap()
        );
        let (a, b) = tokio::join!(
            store.compare_and_swap(b"key", Some(b"first"), b"a"),
            store.compare_and_swap(b"key", Some(b"first"), b"b"),
        );
        assert_ne!(a.unwrap(), b.unwrap());
        let reopened = FileSignerStore::open(&dir).unwrap();
        assert_eq!(
            reopened.get(b"key").await.unwrap(),
            store.get(b"key").await.unwrap()
        );
    }

    #[test]
    fn corrupt_snapshot_and_partial_initialization_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
        atomic_write(&dir.path.join("signer.bin"), b"invalid snapshot").unwrap();
        assert!(
            FileSignerStore::open(&dir)
                .err()
                .unwrap()
                .to_string()
                .contains("损坏")
        );
        assert!(
            dir.is_initialized()
                .unwrap_err()
                .to_string()
                .contains("root-key")
        );
        assert_eq!(
            fs::read(dir.path.join("signer.bin")).unwrap(),
            b"invalid snapshot"
        );
    }

    #[test]
    fn account_lock_excludes_other_openers_and_releases_on_drop() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("account");
        let first = AccountDir::lock(&path).unwrap();
        assert!(AccountDir::lock(&path).is_err());
        drop(first);
        assert!(AccountDir::lock(&path).is_ok());
    }

    #[tokio::test]
    async fn failed_write_stops_further_operations_and_preserves_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let dir = AccountDir::lock(&temp.path().join("account")).unwrap();
        let store = FileSignerStore::new(&dir);
        store.put(b"key", b"original").await.unwrap();
        let path = dir.path.join("signer.bin");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.put(b"key", b"new").await.is_err());
        assert!(store.get(b"key").await.is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            FileSignerStore::open(&dir)
                .unwrap()
                .get(b"key")
                .await
                .unwrap(),
            Some(b"original".to_vec())
        );
    }
}
