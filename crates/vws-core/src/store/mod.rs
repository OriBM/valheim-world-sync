//! The remote: the GitHub data repo ([`github::GitHubStore`]) or a local
//! stand-in with the same semantics ([`dir::DirStore`]).

pub mod dir;
pub mod faulty;
pub mod github;

use std::sync::Arc;

use crate::error::Result;
use crate::util::UnixTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub content: Vec<u8>,
    /// The blob SHA: the compare-and-swap token (I1).
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    pub sha: String,
    /// Commit time on the server's clock, when reported.
    pub commit_time: Option<UnixTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitInfo {
    pub commit_time: UnixTime,
    /// The server's clock at the time of the response (its `Date` header).
    pub server_time: UnixTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub id: u64,
    pub tag: String,
    pub upload_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    /// `uploaded` once complete.
    pub state: String,
    pub created_at: String,
    /// `sha256:<hex>` when the server reports it.
    pub digest: Option<String>,
}

pub trait Store: Send + Sync {
    /// `None` when the file does not exist.
    fn read_file(&self, path: &str) -> Result<Option<RemoteFile>>;
    /// Create (`expected_sha` = `None`) or replace a file.
    /// `Err(Error::Conflict)` when it changed since `expected_sha` was read,
    /// or, for a create, when it already exists.
    fn write_file(&self, path: &str, content: &[u8], expected_sha: Option<&str>, message: &str)
    -> Result<WriteOutcome>;
    fn delete_file(&self, path: &str, sha: &str, message: &str) -> Result<()>;
    /// Time of the latest commit touching `path`, with the server's current time (I6).
    fn last_commit(&self, path: &str) -> Result<Option<CommitInfo>>;
    fn find_release(&self, tag: &str) -> Result<Option<Release>>;
    fn create_release(&self, tag: &str) -> Result<Release>;
    fn list_assets(&self, release: &Release) -> Result<Vec<Asset>>;
    /// Fails with HTTP 422 when an asset of that name exists.
    fn upload_asset(&self, release: &Release, name: &str, data: &[u8]) -> Result<Asset>;
    fn download_asset(&self, asset_id: u64) -> Result<Vec<u8>>;
    fn rename_asset(&self, asset_id: u64, new_name: &str) -> Result<Asset>;
    fn delete_asset(&self, asset_id: u64) -> Result<()>;
}

impl<T: Store + ?Sized> Store for Arc<T> {
    fn read_file(&self, path: &str) -> Result<Option<RemoteFile>> {
        (**self).read_file(path)
    }
    fn write_file(
        &self,
        path: &str,
        content: &[u8],
        expected_sha: Option<&str>,
        message: &str,
    ) -> Result<WriteOutcome> {
        (**self).write_file(path, content, expected_sha, message)
    }
    fn delete_file(&self, path: &str, sha: &str, message: &str) -> Result<()> {
        (**self).delete_file(path, sha, message)
    }
    fn last_commit(&self, path: &str) -> Result<Option<CommitInfo>> {
        (**self).last_commit(path)
    }
    fn find_release(&self, tag: &str) -> Result<Option<Release>> {
        (**self).find_release(tag)
    }
    fn create_release(&self, tag: &str) -> Result<Release> {
        (**self).create_release(tag)
    }
    fn list_assets(&self, release: &Release) -> Result<Vec<Asset>> {
        (**self).list_assets(release)
    }
    fn upload_asset(&self, release: &Release, name: &str, data: &[u8]) -> Result<Asset> {
        (**self).upload_asset(release, name, data)
    }
    fn download_asset(&self, asset_id: u64) -> Result<Vec<u8>> {
        (**self).download_asset(asset_id)
    }
    fn rename_asset(&self, asset_id: u64, new_name: &str) -> Result<Asset> {
        (**self).rename_asset(asset_id, new_name)
    }
    fn delete_asset(&self, asset_id: u64) -> Result<()> {
        (**self).delete_asset(asset_id)
    }
}

/// Remote paths are plain: letters, digits, `.`, `_`, `-`, `/`.
pub(crate) fn check_remote_path(path: &str) -> Result<()> {
    let ok = !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if ok {
        Ok(())
    } else {
        Err(crate::Error::Invalid(format!("unsupported remote path {path:?}")))
    }
}

/// Asset names we create are plain too, so they survive GitHub's renaming rules.
pub(crate) fn check_asset_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(crate::Error::Invalid(format!("unsupported asset name {name:?}")))
    }
}
