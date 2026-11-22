use super::{Api, json_response};
use anyhow::{Context, Result, ensure};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

const CONFIG: &str = "/api/v1/config";
const SCANS: &str = "/api/v1/library/scans";

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Root {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Deserialize)]
pub struct ScanJob {
    pub id: String,
    pub status: String,
    #[serde(flatten)]
    pub counts: ScanCounts,
    pub error: String,
}

impl ScanJob {
    pub(crate) fn is_active(&self) -> bool { matches!(self.status.as_str(), "queued" | "running") }
}

#[derive(Clone, Copy, Deserialize)]
pub struct ScanCounts {
    pub discovered: i64,
    pub processed: i64,
    pub added: i64,
    pub updated: i64,
    pub unavailable: i64,
    pub errors: i64,
}

pub struct LibraryAdmin {
    pub revision: String,
    pub roots: Vec<Root>,
    pub active_scan: Option<ScanJob>,
}

pub enum LibraryUpdate { Settings(LibraryAdmin), Scan(ScanJob), CancelRequested }

pub enum LibraryOp {
    Load,
    Incremental,
    Full,
    Status(String),
    Cancel(String),
    SaveRoots { revision: String, roots: Vec<Root> },
}

#[derive(Deserialize)]
struct ConfigDocument<C> {
    config: C,
    revision: String,
}

#[derive(Deserialize)]
struct RootConfig {
    // The server can encode a nil Go slice as null.
    library_roots: Option<Vec<Root>>,
}

#[derive(Deserialize)]
struct ScanList { items: Vec<ScanJob> }

impl Api {
    pub(super) fn library_admin(&self, action: LibraryOp, read_only: bool) -> Result<LibraryUpdate> {
        ensure!(!read_only || matches!(action, LibraryOp::Load | LibraryOp::Status(_)),
            "Read-only mode cannot change library settings or start or stop scans.");
        ensure!(self.cookie.is_some(), "Sign in to view server library settings.");

        let document = match action {
            LibraryOp::Load => self.library_sources()?,
            LibraryOp::Incremental | LibraryOp::Full => {
                let mode = if matches!(action, LibraryOp::Full) { "full" } else { "incremental" };
                let job = json_response(self.send(Method::POST, SCANS, Some(json!({"mode": mode})), None)?)?;
                return Ok(LibraryUpdate::Scan(job));
            }
            LibraryOp::Status(id) => {
                let job = self.library_scans()?.into_iter().find(|job| job.id == id)
                    .context("Server did not return the requested scan status.")?;
                return Ok(LibraryUpdate::Scan(job));
            }
            LibraryOp::Cancel(id) => {
                ensure!(!id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "Invalid library scan ID.");
                let response = self.send(Method::DELETE, &format!("{SCANS}/{id}"), None, None)?;
                match response.status() {
                    StatusCode::NO_CONTENT => {}
                    status if !status.is_success() => { let _: Value = json_response(response)?; }
                    _ => anyhow::bail!("Unexpected scan cancellation response."),
                }
                return Ok(LibraryUpdate::CancelRequested);
            }
            LibraryOp::SaveRoots { revision, roots } => {
                // Only this request needs the entire config. The server also checks
                // revision in the PUT, closing the race after this GET.
                let ConfigDocument { mut config, revision: current } = self.get::<ConfigDocument<Map<String, Value>>>(CONFIG)?;
                ensure!(!revision.is_empty() && revision == current, "Server configuration changed. Refresh before saving library sources.");
                ensure!(config.contains_key("library_roots"), "Invalid server library configuration.");
                config.insert("library_roots".into(), serde_json::to_value(roots)?);
                let body = Value::Object(Map::from_iter([
                    ("config".into(), Value::Object(config)),
                    ("revision".into(), Value::String(revision)),
                ]));
                json_response(self.send(Method::PUT, CONFIG, Some(body), None)?)?
            }
        };
        ensure!(!document.revision.is_empty(), "Server did not return a configuration revision.");
        let active_scan = self.library_scans()?.into_iter().find(ScanJob::is_active);
        Ok(LibraryUpdate::Settings(LibraryAdmin { revision: document.revision,
            roots: document.config.library_roots.unwrap_or_default(), active_scan }))
    }

    fn library_sources(&self) -> Result<ConfigDocument<RootConfig>> {
        let document: ConfigDocument<RootConfig> = self.get(CONFIG)?;
        ensure!(!document.revision.is_empty(), "Server did not return a configuration revision.");
        Ok(document)
    }

    fn library_scans(&self) -> Result<Vec<ScanJob>> {
        Ok(self.get::<ScanList>(SCANS)?.items)
    }
}
