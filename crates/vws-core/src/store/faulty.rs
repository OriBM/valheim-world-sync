//! Wraps a store and injects failures: corrupted or failed uploads, lost
//! responses, and a hook that runs right after an upload (to take the lock
//! away at the worst moment). Used by the tests for I3 and lost locks; works
//! around GitHub too.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{Asset, CommitInfo, Release, RemoteFile, Store, WriteOutcome};
use crate::error::{Error, Result};

type Hook = Box<dyn FnOnce() + Send>;

pub struct FaultyStore<S> {
    pub inner: S,
    corrupt_uploads: AtomicBool,
    fail_uploads: AtomicBool,
    lose_write_responses: AtomicU32,
    after_upload: Mutex<Vec<Hook>>,
}

impl<S: Store> FaultyStore<S> {
    pub fn new(inner: S) -> Self {
        FaultyStore {
            inner,
            corrupt_uploads: AtomicBool::new(false),
            fail_uploads: AtomicBool::new(false),
            lose_write_responses: AtomicU32::new(0),
            after_upload: Mutex::new(Vec::new()),
        }
    }

    /// Uploads store different bytes than they were given.
    pub fn corrupt_uploads(&self, on: bool) {
        self.corrupt_uploads.store(on, Ordering::SeqCst);
    }

    /// Uploads fail with a network error before anything is stored.
    pub fn fail_uploads(&self, on: bool) {
        self.fail_uploads.store(on, Ordering::SeqCst);
    }

    /// The next `n` file writes succeed on the server, but the caller sees a network error.
    pub fn lose_write_responses(&self, n: u32) {
        self.lose_write_responses.store(n, Ordering::SeqCst);
    }

    /// Run `f` once, right after the next successful upload.
    pub fn after_next_upload(&self, f: impl FnOnce() + Send + 'static) {
        self.after_upload.lock().unwrap().push(Box::new(f));
    }
}

impl<S: Store> Store for FaultyStore<S> {
    fn read_file(&self, path: &str) -> Result<Option<RemoteFile>> {
        self.inner.read_file(path)
    }

    fn write_file(
        &self,
        path: &str,
        content: &[u8],
        expected_sha: Option<&str>,
        message: &str,
    ) -> Result<WriteOutcome> {
        let out = self.inner.write_file(path, content, expected_sha, message)?;
        let lose = self
            .lose_write_responses
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if lose {
            return Err(Error::Network("injected: response lost after the write landed".into()));
        }
        Ok(out)
    }

    fn delete_file(&self, path: &str, sha: &str, message: &str) -> Result<()> {
        self.inner.delete_file(path, sha, message)
    }

    fn last_commit(&self, path: &str) -> Result<Option<CommitInfo>> {
        self.inner.last_commit(path)
    }

    fn find_release(&self, tag: &str) -> Result<Option<Release>> {
        self.inner.find_release(tag)
    }

    fn create_release(&self, tag: &str) -> Result<Release> {
        self.inner.create_release(tag)
    }

    fn list_assets(&self, release: &Release) -> Result<Vec<Asset>> {
        self.inner.list_assets(release)
    }

    fn upload_asset(&self, release: &Release, name: &str, data: &[u8]) -> Result<Asset> {
        if self.fail_uploads.load(Ordering::SeqCst) {
            return Err(Error::Network("injected: upload failed".into()));
        }
        let asset = if self.corrupt_uploads.load(Ordering::SeqCst) && !data.is_empty() {
            let mut bad = data.to_vec();
            let mid = bad.len() / 2;
            bad[mid] ^= 0xFF;
            self.inner.upload_asset(release, name, &bad)?
        } else {
            self.inner.upload_asset(release, name, data)?
        };
        let hooks: Vec<Hook> = std::mem::take(&mut *self.after_upload.lock().unwrap());
        for h in hooks {
            h();
        }
        Ok(asset)
    }

    fn download_asset(&self, asset_id: u64) -> Result<Vec<u8>> {
        self.inner.download_asset(asset_id)
    }

    fn rename_asset(&self, asset_id: u64, new_name: &str) -> Result<Asset> {
        self.inner.rename_asset(asset_id, new_name)
    }

    fn delete_asset(&self, asset_id: u64) -> Result<()> {
        self.inner.delete_asset(asset_id)
    }
}
