//! The GitHub data repo over the REST API (PLAN.md 2.5).
//!
//! - Blocking; one instance per PC. Every mutation goes through one pacer,
//!   serially and about a second apart (I8).
//! - The token is sent only to api.github.com and uploads.github.com. Asset
//!   downloads that redirect are followed without it.
//! - `last_commit` uses conditional requests, so polling an unchanged file is
//!   answered with 304 and does not count against the rate limit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;
use ureq::http::HeaderMap;

use super::{Asset, CommitInfo, Release, RemoteFile, Store, WriteOutcome, check_asset_name, check_remote_path};
use crate::error::{Error, Result};
use crate::setup_code::{Secret, SetupCode, validate_repo_part};
use crate::util::{self, UnixTime};

const API: &str = "https://api.github.com";
const UPLOADS: &str = "https://uploads.github.com";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = concat!("valheim-world-sync/", env!("CARGO_PKG_VERSION"));
const JSON: &str = "application/vnd.github+json";
const OCTETS: &str = "application/octet-stream";
const MAX_JSON: u64 = 16 * 1024 * 1024;
const MAX_ASSET: u64 = 2 * 1024 * 1024 * 1024;

pub struct GitHubStore {
    agent: ureq::Agent,
    owner: String,
    repo: String,
    token: Secret,
    pacer: Pacer,
    /// path -> (ETag, commit time) of the last `last_commit` answer.
    commit_cache: Mutex<HashMap<String, (String, UnixTime)>>,
    server_time: Mutex<Option<UnixTime>>,
    token_expiry: Mutex<Option<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Get,
    Put,
    Post,
    Patch,
    Delete,
}

struct Req<'a> {
    method: Method,
    url: String,
    accept: &'a str,
    body: Option<(&'a str, &'a [u8])>,
    if_none_match: Option<String>,
    auth: bool,
    max_body: u64,
}

impl<'a> Req<'a> {
    fn get(url: String) -> Self {
        Req {
            method: Method::Get,
            url,
            accept: JSON,
            body: None,
            if_none_match: None,
            auth: true,
            max_body: MAX_JSON,
        }
    }
    fn with_body(method: Method, url: String, content_type: &'a str, body: &'a [u8]) -> Self {
        Req {
            method,
            body: Some((content_type, body)),
            ..Req::get(url)
        }
    }
}

struct Resp {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
    fn json<T: for<'de> Deserialize<'de>>(&self) -> Result<T> {
        serde_json::from_slice(&self.body).map_err(|e| Error::Invalid(format!("unexpected answer from GitHub: {e}")))
    }
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Serializes mutations and keeps them about a second apart (I8).
struct Pacer {
    gap: Duration,
    last: Mutex<Option<Instant>>,
}

impl Pacer {
    fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < self.gap {
                std::thread::sleep(self.gap - since);
            }
        }
        let out = f();
        *last = Some(Instant::now());
        out
    }
}

#[derive(Deserialize)]
struct AssetJson {
    id: u64,
    name: String,
    size: u64,
    state: String,
    created_at: String,
    #[serde(default)]
    digest: Option<String>,
}

impl From<AssetJson> for Asset {
    fn from(a: AssetJson) -> Self {
        Asset {
            id: a.id,
            name: a.name,
            size: a.size,
            state: a.state,
            created_at: a.created_at,
            digest: a.digest,
        }
    }
}

#[derive(Deserialize)]
struct ReleaseJson {
    id: u64,
    tag_name: String,
    #[serde(default)]
    upload_url: String,
}

#[derive(Deserialize)]
struct CommitterJson {
    date: String,
}

#[derive(Deserialize)]
struct CommitJson {
    committer: CommitterJson,
}

impl GitHubStore {
    pub fn new(owner: &str, repo: &str, token: Secret) -> Result<Self> {
        validate_repo_part(owner)?;
        validate_repo_part(repo)?;
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(USER_AGENT)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(90)))
            .timeout_per_write(Some(Duration::from_secs(90)))
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .build();
        Ok(GitHubStore {
            agent: config.into(),
            owner: owner.into(),
            repo: repo.into(),
            token,
            pacer: Pacer {
                gap: Duration::from_millis(1100),
                last: Mutex::new(None),
            },
            commit_cache: Mutex::new(HashMap::new()),
            server_time: Mutex::new(None),
            token_expiry: Mutex::new(None),
        })
    }

    pub fn from_setup_code(code: &SetupCode) -> Result<Self> {
        Self::new(&code.owner, &code.repo, code.token.clone())
    }

    /// `owner/repo`
    pub fn repo_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// The token's expiry as GitHub reports it, for the warning in PLAN.md 7.8.
    pub fn token_expiry(&self) -> Option<String> {
        self.token_expiry.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// GitHub's clock, from the latest response.
    pub fn server_time(&self) -> Option<UnixTime> {
        *self.server_time.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Check that the repository exists and the token can see it.
    pub fn check_access(&self) -> Result<()> {
        let resp = self.send(Req::get(format!("{API}/repos/{}/{}", self.owner, self.repo)))?;
        match resp.status {
            200 => Ok(()),
            404 => Err(Error::NotFound(format!(
                "repository {} (it does not exist, or the token cannot see it)",
                self.repo_name()
            ))),
            _ => Err(fail(&resp)),
        }
    }

    fn repo_url(&self, tail: &str) -> String {
        format!("{API}/repos/{}/{}/{tail}", self.owner, self.repo)
    }

    fn send(&self, r: Req<'_>) -> Result<Resp> {
        if r.method == Method::Get {
            self.send_now(&r)
        } else {
            self.pacer.run(|| self.send_now(&r))
        }
    }

    fn decorate<B>(&self, mut rb: ureq::RequestBuilder<B>, r: &Req<'_>, auth: &str) -> ureq::RequestBuilder<B> {
        rb = rb.header("Accept", r.accept);
        if r.auth {
            rb = rb
                .header("Authorization", auth)
                .header("X-GitHub-Api-Version", API_VERSION);
        }
        if let Some(etag) = &r.if_none_match {
            rb = rb.header("If-None-Match", etag.as_str());
        }
        rb
    }

    fn send_now(&self, r: &Req<'_>) -> Result<Resp> {
        let auth = format!("Bearer {}", self.token.expose());
        let url = r.url.as_str();
        let result = match (r.method, r.body) {
            (Method::Get, _) => self.decorate(self.agent.get(url), r, &auth).call(),
            (Method::Delete, None) => self.decorate(self.agent.delete(url), r, &auth).call(),
            (Method::Delete, Some((ct, b))) => self
                .decorate(self.agent.delete(url).force_send_body(), r, &auth)
                .header("Content-Type", ct)
                .send(b),
            (Method::Put, Some((ct, b))) => self
                .decorate(self.agent.put(url), r, &auth)
                .header("Content-Type", ct)
                .send(b),
            (Method::Post, Some((ct, b))) => self
                .decorate(self.agent.post(url), r, &auth)
                .header("Content-Type", ct)
                .send(b),
            (Method::Patch, Some((ct, b))) => self
                .decorate(self.agent.patch(url), r, &auth)
                .header("Content-Type", ct)
                .send(b),
            (m, None) => return Err(Error::Invalid(format!("{m:?} needs a body"))),
        };
        let mut resp = result.map_err(|e| Error::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        if let Some(t) = headers
            .get("date")
            .and_then(|v| v.to_str().ok())
            .and_then(util::parse_http_date)
        {
            *self.server_time.lock().unwrap_or_else(|p| p.into_inner()) = Some(t);
        }
        if let Some(x) = headers
            .get("github-authentication-token-expiration")
            .and_then(|v| v.to_str().ok())
        {
            *self.token_expiry.lock().unwrap_or_else(|p| p.into_inner()) = Some(x.to_string());
        }
        let body = resp
            .body_mut()
            .with_config()
            .limit(r.max_body)
            .read_to_vec()
            .map_err(|e| Error::Network(e.to_string()))?;
        Ok(Resp { status, headers, body })
    }

    fn response_time(&self, resp: &Resp) -> Result<UnixTime> {
        resp.header("date")
            .and_then(util::parse_http_date)
            .ok_or_else(|| Error::Invalid("GitHub's answer has no usable Date header".into()))
    }
}

/// Turn a non-success answer into an error. GitHub's messages never contain the token.
fn fail(resp: &Resp) -> Error {
    #[derive(Deserialize)]
    struct Msg {
        message: Option<String>,
    }
    let message = serde_json::from_slice::<Msg>(&resp.body)
        .ok()
        .and_then(|m| m.message)
        .unwrap_or_default();
    let retry_after = resp.header("retry-after").and_then(|s| s.trim().parse::<u64>().ok());
    let exhausted = resp.header("x-ratelimit-remaining") == Some("0");
    let reset_in = resp
        .header("x-ratelimit-reset")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .zip(resp.header("date").and_then(util::parse_http_date))
        .map(|(reset, now)| (reset - now).max(0) as u64);
    match resp.status {
        401 => Error::Unauthorized,
        403 | 429 if retry_after.is_some() || exhausted || message.to_ascii_lowercase().contains("rate limit") => {
            Error::RateLimited {
                retry_after: retry_after.or(reset_in),
            }
        }
        404 => Error::NotFound(if message.is_empty() {
            "Not Found".into()
        } else {
            message
        }),
        409 => Error::Conflict,
        status => Error::Http {
            status,
            message: message.chars().take(300).collect(),
        },
    }
}

impl Store for GitHubStore {
    fn read_file(&self, path: &str) -> Result<Option<RemoteFile>> {
        check_remote_path(path)?;
        let resp = self.send(Req::get(self.repo_url(&format!("contents/{path}"))))?;
        if resp.status == 404 {
            return Ok(None);
        }
        if !resp.ok() {
            return Err(fail(&resp));
        }
        #[derive(Deserialize)]
        struct Contents {
            #[serde(rename = "type")]
            kind: String,
            #[serde(default)]
            encoding: Option<String>,
            #[serde(default)]
            content: Option<String>,
            sha: String,
        }
        let c: Contents = resp.json()?;
        if c.kind != "file" {
            return Err(Error::Invalid(format!("{path} on GitHub is not a file")));
        }
        if c.encoding.as_deref() != Some("base64") {
            return Err(Error::Invalid(format!(
                "{path} is too large to read through the contents API"
            )));
        }
        let b64: String = c
            .content
            .unwrap_or_default()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        let content = STANDARD
            .decode(b64)
            .map_err(|_| Error::Invalid(format!("GitHub sent {path} in a form that does not decode")))?;
        Ok(Some(RemoteFile { content, sha: c.sha }))
    }

    fn write_file(
        &self,
        path: &str,
        content: &[u8],
        expected_sha: Option<&str>,
        message: &str,
    ) -> Result<WriteOutcome> {
        check_remote_path(path)?;
        let mut body = json!({ "message": message, "content": STANDARD.encode(content) });
        if let Some(sha) = expected_sha {
            body["sha"] = json!(sha);
        }
        let bytes = serde_json::to_vec(&body)?;
        let resp = self.send(Req::with_body(
            Method::Put,
            self.repo_url(&format!("contents/{path}")),
            "application/json",
            &bytes,
        ))?;
        match resp.status {
            200 | 201 => {
                #[derive(Deserialize)]
                struct ShaJson {
                    sha: String,
                }
                #[derive(Deserialize)]
                struct Out {
                    content: ShaJson,
                    commit: CommitJson,
                }
                let out: Out = resp.json()?;
                Ok(WriteOutcome {
                    sha: out.content.sha,
                    commit_time: util::parse_rfc3339(&out.commit.committer.date),
                })
            }
            409 => Err(Error::Conflict),
            // Creating without a sha while the file exists.
            422 if expected_sha.is_none() => Err(Error::Conflict),
            _ => Err(fail(&resp)),
        }
    }

    fn delete_file(&self, path: &str, sha: &str, message: &str) -> Result<()> {
        check_remote_path(path)?;
        let bytes = serde_json::to_vec(&json!({ "message": message, "sha": sha }))?;
        let resp = self.send(Req::with_body(
            Method::Delete,
            self.repo_url(&format!("contents/{path}")),
            "application/json",
            &bytes,
        ))?;
        match resp.status {
            200 => Ok(()),
            409 => Err(Error::Conflict),
            _ => Err(fail(&resp)),
        }
    }

    fn last_commit(&self, path: &str) -> Result<Option<CommitInfo>> {
        check_remote_path(path)?;
        let cached = self
            .commit_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(path)
            .cloned();
        let mut req = Req::get(self.repo_url(&format!("commits?path={path}&per_page=1")));
        req.if_none_match = cached.as_ref().map(|(etag, _)| etag.clone());
        let resp = self.send(req)?;
        let server_time = self.response_time(&resp)?;
        match resp.status {
            304 => {
                let (_, commit_time) = cached.ok_or_else(|| Error::Invalid("304 without a cached answer".into()))?;
                Ok(Some(CommitInfo {
                    commit_time,
                    server_time,
                }))
            }
            200 => {
                #[derive(Deserialize)]
                struct Item {
                    commit: CommitJson,
                }
                let items: Vec<Item> = resp.json()?;
                let mut cache = self.commit_cache.lock().unwrap_or_else(|p| p.into_inner());
                let Some(first) = items.first() else {
                    cache.remove(path);
                    return Ok(None);
                };
                let commit_time = util::parse_rfc3339(&first.commit.committer.date)
                    .ok_or_else(|| Error::Invalid("GitHub sent a commit date that does not parse".into()))?;
                if let Some(etag) = resp.header("etag") {
                    cache.insert(path.to_string(), (etag.to_string(), commit_time));
                }
                Ok(Some(CommitInfo {
                    commit_time,
                    server_time,
                }))
            }
            _ => Err(fail(&resp)),
        }
    }

    fn find_release(&self, tag: &str) -> Result<Option<Release>> {
        check_remote_path(tag)?;
        let resp = self.send(Req::get(self.repo_url(&format!("releases/tags/{tag}"))))?;
        match resp.status {
            404 => Ok(None),
            200 => {
                let r: ReleaseJson = resp.json()?;
                Ok(Some(Release {
                    id: r.id,
                    tag: r.tag_name,
                    upload_url: r.upload_url,
                }))
            }
            _ => Err(fail(&resp)),
        }
    }

    fn create_release(&self, tag: &str) -> Result<Release> {
        check_remote_path(tag)?;
        let bytes = serde_json::to_vec(&json!({
            "tag_name": tag,
            "name": "Saves",
            "body": "World saves for Valheim World Sync. Managed by the app; do not edit by hand.",
            "draft": false,
            "prerelease": false,
            "make_latest": "false",
        }))?;
        let resp = self.send(Req::with_body(
            Method::Post,
            self.repo_url("releases"),
            "application/json",
            &bytes,
        ))?;
        if resp.status != 201 {
            return Err(fail(&resp));
        }
        let r: ReleaseJson = resp.json()?;
        Ok(Release {
            id: r.id,
            tag: r.tag_name,
            upload_url: r.upload_url,
        })
    }

    fn list_assets(&self, release: &Release) -> Result<Vec<Asset>> {
        let mut out = Vec::new();
        for page in 1..=50 {
            let resp = self.send(Req::get(
                self.repo_url(&format!("releases/{}/assets?per_page=100&page={page}", release.id)),
            ))?;
            if !resp.ok() {
                return Err(fail(&resp));
            }
            let items: Vec<AssetJson> = resp.json()?;
            let n = items.len();
            out.extend(items.into_iter().map(Asset::from));
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    fn upload_asset(&self, release: &Release, name: &str, data: &[u8]) -> Result<Asset> {
        check_asset_name(name)?;
        // Built from a constant, never from the API's upload_url, so the token only goes to GitHub.
        let url = format!(
            "{UPLOADS}/repos/{}/{}/releases/{}/assets?name={name}",
            self.owner, self.repo, release.id
        );
        let resp = self.send(Req::with_body(Method::Post, url, "application/zip", data))?;
        if resp.status != 201 {
            return Err(fail(&resp));
        }
        Ok(resp.json::<AssetJson>()?.into())
    }

    fn download_asset(&self, asset_id: u64) -> Result<Vec<u8>> {
        let req = Req {
            accept: OCTETS,
            max_body: MAX_ASSET,
            ..Req::get(self.repo_url(&format!("releases/assets/{asset_id}")))
        };
        let resp = self.send(req)?;
        match resp.status {
            200 => Ok(resp.body),
            301 | 302 | 303 | 307 | 308 => {
                let location = resp
                    .header("location")
                    .filter(|l| l.starts_with("https://"))
                    .ok_or_else(|| Error::Invalid("GitHub redirected the download somewhere unexpected".into()))?
                    .to_string();
                let follow = Req {
                    accept: OCTETS,
                    auth: false, // never forward the token to the redirect target
                    max_body: MAX_ASSET,
                    ..Req::get(location)
                };
                let resp = self.send(follow)?;
                if resp.status == 200 {
                    Ok(resp.body)
                } else {
                    Err(fail(&resp))
                }
            }
            _ => Err(fail(&resp)),
        }
    }

    fn rename_asset(&self, asset_id: u64, new_name: &str) -> Result<Asset> {
        check_asset_name(new_name)?;
        let bytes = serde_json::to_vec(&json!({ "name": new_name }))?;
        let resp = self.send(Req::with_body(
            Method::Patch,
            self.repo_url(&format!("releases/assets/{asset_id}")),
            "application/json",
            &bytes,
        ))?;
        if resp.status != 200 {
            return Err(fail(&resp));
        }
        Ok(resp.json::<AssetJson>()?.into())
    }

    fn delete_asset(&self, asset_id: u64) -> Result<()> {
        let resp = self.send(Req {
            method: Method::Delete,
            ..Req::get(self.repo_url(&format!("releases/assets/{asset_id}")))
        })?;
        match resp.status {
            204 => Ok(()),
            _ => Err(fail(&resp)),
        }
    }
}

impl std::fmt::Debug for GitHubStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubStore")
            .field("repo", &self.repo_name())
            .field("token", &self.token)
            .finish()
    }
}
