use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Content hashes of files as last read or written in this session.
///
/// Edit tools consult it so a blind or stale edit is rejected with guidance
/// the model can act on. One ledger is shared by all tools of a registry.
pub struct FileLedger {
    workspace_root: PathBuf,
    entries: Mutex<HashMap<PathBuf, u64>>,
}

impl FileLedger {
    pub fn new(workspace_root: PathBuf) -> Self {
        let workspace_root = workspace_root.canonicalize().unwrap_or(workspace_root);
        Self {
            workspace_root,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Remember `content` as the current state of `path`.
    pub fn record(&self, path: &Path, content: &str) {
        let key = resolve(path);
        let hash = hash_content(content);
        self.lock().insert(key, hash);
    }

    /// Allow an edit only if `path` was read or written in this session and
    /// `current_content` (what is on disk now) is still what was recorded.
    pub fn check_edit(&self, path: &Path, current_content: &str) -> Result<(), String> {
        let key = resolve(path);
        let recorded = self.lock().get(&key).copied();
        match recorded {
            None => Err(format!(
                "`{}` has not been read in this session. Read it with read_file before editing it.",
                self.relative(&key)
            )),
            Some(hash) if hash != hash_content(current_content) => Err(format!(
                "`{}` changed since it was last read. Read it again with read_file before editing it.",
                self.relative(&key)
            )),
            Some(_) => Ok(()),
        }
    }

    /// Update the hash of a file that is already tracked; untracked paths stay untracked.
    pub fn refresh_if_tracked(&self, path: &Path, content: &str) {
        let key = resolve(path);
        let mut entries = self.lock();
        if let Some(entry) = entries.get_mut(&key) {
            *entry = hash_content(content);
        }
    }

    /// Like `refresh_if_tracked`, reading the content from disk. Drops the
    /// entry when the file can no longer be read as text.
    pub fn refresh_from_disk(&self, path: &Path) {
        let key = resolve(path);
        if !self.lock().contains_key(&key) {
            return;
        }
        match std::fs::read_to_string(&key) {
            Ok(content) => self.refresh_if_tracked(&key, &content),
            Err(_) => self.forget(&key),
        }
    }

    /// Drop `path` and everything below it (used after delete and rename).
    pub fn forget(&self, path: &Path) {
        let key = resolve(path);
        self.lock().retain(|tracked, _| !tracked.starts_with(&key));
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<PathBuf, u64>> {
        // The map stays valid even if a holder panicked.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn relative(&self, key: &Path) -> String {
        key.strip_prefix(&self.workspace_root)
            .unwrap_or(key)
            .display()
            .to_string()
    }
}

/// Canonical form of `path`. A missing file keeps its canonical parent so
/// entries for deleted files still match.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn hash_content(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(content.as_bytes());
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, FileLedger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = FileLedger::new(dir.path().to_path_buf());
        (dir, ledger)
    }

    #[test]
    fn recorded_content_passes_the_check() {
        let (dir, ledger) = setup();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();

        ledger.record(&path, "one");

        assert!(ledger.check_edit(&path, "one").is_ok());
    }

    #[test]
    fn unrecorded_file_is_rejected_with_a_relative_path() {
        let (dir, ledger) = setup();
        let path = dir.path().join("src").join("a.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "one").unwrap();

        let err = ledger.check_edit(&path, "one").unwrap_err();

        assert_eq!(
            err,
            format!(
                "`{}` has not been read in this session. Read it with read_file before editing it.",
                Path::new("src").join("a.txt").display()
            )
        );
    }

    #[test]
    fn changed_content_is_rejected() {
        let (dir, ledger) = setup();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "one").unwrap();
        ledger.record(&path, "one");

        let err = ledger.check_edit(&path, "two").unwrap_err();

        assert_eq!(
            err,
            "`a.txt` changed since it was last read. Read it again with read_file before editing it."
        );
    }

    #[test]
    fn refresh_if_tracked_ignores_untracked_paths() {
        let (dir, ledger) = setup();
        let tracked = dir.path().join("tracked.txt");
        let untracked = dir.path().join("untracked.txt");
        std::fs::write(&tracked, "old").unwrap();
        std::fs::write(&untracked, "old").unwrap();
        ledger.record(&tracked, "old");

        ledger.refresh_if_tracked(&tracked, "new");
        ledger.refresh_if_tracked(&untracked, "new");

        assert!(ledger.check_edit(&tracked, "new").is_ok());
        assert!(ledger.check_edit(&untracked, "new").is_err());
    }

    #[test]
    fn refresh_from_disk_follows_the_file_and_drops_unreadable_entries() {
        let (dir, ledger) = setup();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "old").unwrap();
        ledger.record(&path, "old");

        std::fs::write(&path, "new").unwrap();
        ledger.refresh_from_disk(&path);
        assert!(ledger.check_edit(&path, "new").is_ok());

        std::fs::remove_file(&path).unwrap();
        ledger.refresh_from_disk(&path);
        std::fs::write(&path, "new").unwrap();
        assert!(ledger.check_edit(&path, "new").is_err());
    }

    #[test]
    fn forget_drops_a_file_and_everything_below_a_directory() {
        let (dir, ledger) = setup();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let inside = sub.join("a.txt");
        let sibling = dir.path().join("sub2.txt");
        std::fs::write(&inside, "x").unwrap();
        std::fs::write(&sibling, "x").unwrap();
        ledger.record(&inside, "x");
        ledger.record(&sibling, "x");

        ledger.forget(&sub);

        assert!(ledger.check_edit(&inside, "x").is_err());
        assert!(ledger.check_edit(&sibling, "x").is_ok());
    }

    #[test]
    fn non_canonical_paths_share_one_entry() {
        let (dir, ledger) = setup();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();

        ledger.record(&dir.path().join("sub").join("..").join("a.txt"), "x");

        assert!(ledger.check_edit(&path, "x").is_ok());
    }
}
