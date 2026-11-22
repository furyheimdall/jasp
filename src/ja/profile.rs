use anyhow::{Context, Result, ensure};
use reqwest::{Url, header::HeaderValue};
use serde::{Deserialize, Serialize};
use std::{env, fs::{self, DirBuilder, File, OpenOptions}, io::{ErrorKind, Read}, os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt}, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use zeroize::Zeroize;

#[derive(Clone)]
pub(super) struct Store(PathBuf);

#[derive(Deserialize, Serialize)]
pub(super) struct Profile {
    server: String,
    server_id: String,
    allow_http: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cookie: Option<String>,
}

impl Drop for Profile {
    fn drop(&mut self) { self.cookie.zeroize(); }
}

impl Profile {
    pub(super) fn server(&self) -> &str { &self.server }
    pub(super) fn allow_http(&self) -> bool { self.allow_http }
    pub(super) fn matches_url(&self, url: &str) -> bool {
        super::server_url(url).is_ok_and(|url| url.as_str() == self.server)
    }

    fn matches(&self, base: &Url, server_id: &str) -> bool {
        self.server == base.as_str() && self.server_id == server_id
    }

    pub(super) fn cookie(&self, base: &Url, server_id: &str) -> Option<HeaderValue> {
        if !self.matches(base, server_id) || (base.scheme() == "http" && !self.allow_http) { return None; }
        let raw = self.cookie.as_deref()?;
        let token = raw.strip_prefix("jastreamer_session=")?;
        if token.is_empty() || token.len() > 128 || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return None;
        }
        let mut value = HeaderValue::from_str(raw).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

impl Store {
    pub(super) fn for_user() -> Result<Self> {
        let absolute = |key| env::var_os(key).map(PathBuf::from).filter(|p| p.is_absolute());
        let state = absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|p| p.join(".local/state")))
            .context("No private user state directory is configured.")?;
        Ok(Self(state.join(env!("CARGO_PKG_NAME"))))
    }

    pub(super) fn load(&self) -> Result<Option<Profile>> {
        if !self.private_dir(false)? { return Ok(None); }
        let path = self.0.join("jastreamer.json");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(metadata.is_file() && metadata.mode() & 0o077 == 0 && metadata.nlink() == 1,
            "The jastreamer profile is not a private regular file.");
        let file = File::open(path)?;
        let actual = file.metadata()?;
        ensure!(actual.dev() == metadata.dev() && actual.ino() == metadata.ino(), "The jastreamer profile changed while opening.");
        let profile: Profile = serde_json::from_reader(file.take(8192))
            .map_err(|_| anyhow::anyhow!("The saved jastreamer profile is invalid."))?;
        ensure!(profile.matches_url(profile.server()) && !profile.server_id.is_empty(), "The saved jastreamer server is invalid.");
        Ok(Some(profile))
    }

    pub(super) fn save(&self, base: &Url, server_id: &str, allow_http: bool, cookie: &HeaderValue) -> Result<()> {
        let profile = Profile {
            server: base.to_string(), server_id: server_id.to_owned(), allow_http,
            cookie: Some(cookie.to_str().map_err(|_| anyhow::anyhow!("Invalid jastreamer session header."))?.to_owned()),
        };
        self.write(&profile)
    }

    pub(super) fn clear(&self, base: &Url, server_id: &str, cookie: &HeaderValue) -> Result<()> {
        let Some(mut saved) = self.load()? else { return Ok(()); };
        if saved.matches(base, server_id) && saved.cookie.as_deref().is_some_and(|c| c.as_bytes() == cookie.as_bytes()) {
            saved.cookie.zeroize();
            saved.cookie = None;
            self.write(&saved)?;
        }
        Ok(())
    }

    fn private_dir(&self, create: bool) -> Result<bool> {
        let metadata = match fs::symlink_metadata(&self.0) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound && !create => return Ok(false),
            Err(err) if err.kind() == ErrorKind::NotFound => {
                fs::create_dir_all(self.0.parent().expect("application state parent"))?;
                match DirBuilder::new().mode(0o700).create(&self.0) {
                    Ok(()) => {},
                    Err(err) if err.kind() == ErrorKind::AlreadyExists => {},
                    Err(err) => return Err(err.into()),
                }
                fs::symlink_metadata(&self.0)?
            }
            Err(err) => return Err(err.into()),
        };
        ensure!(metadata.is_dir() && metadata.mode() & 0o077 == 0,
            "The moOde Native state directory must be private (0700).");
        Ok(true)
    }

    fn write(&self, profile: &Profile) -> Result<()> {
        self.private_dir(true)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = self.0.join(format!(".jastreamer-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
        let result = (|| -> Result<()> {
            serde_json::to_writer(&mut file, profile).context("writing the private jastreamer profile")?;
            file.sync_all()?;
            fs::rename(&temp, self.0.join("jastreamer.json"))?;
            Ok(())
        })();
        if result.is_err() { let _ = fs::remove_file(&temp); }
        result
    }
}
