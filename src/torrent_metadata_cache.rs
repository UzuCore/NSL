//! Local, bounded metadata-only cache. All hits are decoded and infohash-
//! verified again by the engine before use. Never stores payload files.
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime},
};

const MAX_ENTRY: u64 = 64 * 1024 * 1024;
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 1024;
const TTL: Duration = Duration::from_secs(7 * 86400);

#[derive(Default)]
pub(super) struct MetadataCache {
    root: Option<PathBuf>,
    locks: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    disk: Mutex<()>,
}

fn valid_key(key: &str) -> bool {
    key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

impl MetadataCache {
    pub(super) fn new(download_root: &Path) -> Self {
        let root = (|| {
            let app = download_root.parent()?.canonicalize().ok()?;
            let caches = app.join("caches");
            fs::create_dir_all(&caches).ok()?;
            if caches.canonicalize().ok()? != caches {
                return None;
            }
            let dir = caches.join("torrent-meta-v1");
            fs::create_dir_all(&dir).ok()?;
            if dir.canonicalize().ok()? != dir {
                return None;
            }
            Some(dir)
        })();
        Self {
            root,
            ..Default::default()
        }
    }

    pub(super) fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let Ok(mut map) = self.locks.lock() else {
            return Arc::default();
        };
        map.retain(|_, value| value.strong_count() > 0);
        if let Some(existing) = map.get(key).and_then(Weak::upgrade) {
            return existing;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        map.insert(key.to_owned(), Arc::downgrade(&lock));
        lock
    }

    fn checked_root(&self) -> Option<&Path> {
        let root = self.root.as_deref()?;
        (root.canonicalize().ok()?.as_path() == root).then_some(root)
    }

    fn read(&self, key: &str) -> Option<Vec<u8>> {
        if !valid_key(key) {
            return None;
        }
        let _guard = self.disk.lock().ok()?;
        let root = self.checked_root()?;
        let path = root.join(format!("{key}.torrent"));
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.file_type().is_file()
            || meta.len() == 0
            || meta.len() > MAX_ENTRY
            || meta.modified().ok()?.elapsed().ok()? > TTL
        {
            return None;
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .ok()?
            .take(MAX_ENTRY + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 == meta.len()).then_some(bytes)
    }

    fn store(&self, key: &str, bytes: &[u8]) -> Option<()> {
        if !valid_key(key) || bytes.is_empty() || bytes.len() as u64 > MAX_ENTRY {
            return None;
        }
        let _guard = self.disk.lock().ok()?;
        let root = self.checked_root()?;
        let dest = root.join(format!("{key}.torrent"));
        if fs::symlink_metadata(&dest).is_ok_and(|m| !m.file_type().is_file()) {
            return None;
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(root).ok()?.flatten() {
            let path = entry.path();
            if path == dest
                || path.extension().and_then(|e| e.to_str()) != Some("torrent")
                || !path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(valid_key)
            {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.file_type().is_file() {
                continue;
            }
            files.push((
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                meta.len(),
                path,
            ));
        }
        files.sort_by_key(|item| item.0);
        let mut total = files
            .iter()
            .map(|item| item.1)
            .fold(0u64, u64::saturating_add);
        let mut count = files.len();
        for (modified, size, path) in files {
            let should_remove = modified.elapsed().is_ok_and(|age| age > TTL)
                || total.saturating_add(bytes.len() as u64) > MAX_BYTES
                || count >= MAX_ENTRIES;
            if should_remove && fs::remove_file(path).is_ok() {
                total = total.saturating_sub(size);
                count = count.saturating_sub(1);
            }
        }
        if total.saturating_add(bytes.len() as u64) > MAX_BYTES || count >= MAX_ENTRIES {
            return None;
        }
        let mut file = tempfile::Builder::new()
            .prefix(".nsl-meta-")
            .suffix(".tmp")
            .tempfile_in(root)
            .ok()?;
        file.write_all(bytes).ok()?;
        file.as_file_mut().sync_all().ok()?;
        file.persist(dest).ok()?;
        Some(())
    }

    pub(super) async fn read_async(self: &Arc<Self>, key: String) -> Option<Vec<u8>> {
        let cache = self.clone();
        tokio::task::spawn_blocking(move || cache.read(&key))
            .await
            .ok()
            .flatten()
    }

    pub(super) async fn store_async(self: &Arc<Self>, key: String, bytes: Vec<u8>) {
        let cache = self.clone();
        let _ = tokio::task::spawn_blocking(move || cache.store(&key, &bytes)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stage55_cache_persists_and_rejects_unsafe_keys() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        fs::create_dir(&downloads).unwrap();
        let key = "0123456789abcdef0123456789abcdef01234567";
        let cache = MetadataCache::new(&downloads);
        assert!(cache.store("../outside", b"bad").is_none());
        assert!(cache.store(key, b"metadata fixture").is_some());
        let next = MetadataCache::new(&downloads);
        assert_eq!(next.read(key).unwrap(), b"metadata fixture");
        assert!(next.read("../outside").is_none());
        let first = next.lock_for(key);
        let second = next.lock_for(key);
        assert!(Arc::ptr_eq(&first, &second));
    }
}
