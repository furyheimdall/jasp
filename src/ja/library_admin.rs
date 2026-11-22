use super::{Api, json_response};
use anyhow::{Context, Result, ensure};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

const CONFIG: &str = "/api/v1/config";
const SCANS: &str = "/api/v1/library/scans";

#[derive(Deserialize)]
pub struct Root {
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
}

#[derive(Deserialize)]
struct ConfigDocument<C> {
    config: C,
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
            "Read-only mode cannot start or stop scans.");
        ensure!(self.cookie.is_some(), "Sign in to view server library settings.");

        match action {
            LibraryOp::Load => {
                let document: ConfigDocument<RootConfig> = self.get(CONFIG)?;
                let active_scan = self.library_scans()?.into_iter().find(ScanJob::is_active);
                Ok(LibraryUpdate::Settings(LibraryAdmin { roots: document.config.library_roots.unwrap_or_default(), active_scan }))
            }
            LibraryOp::Incremental | LibraryOp::Full => {
                let mode = if matches!(action, LibraryOp::Full) { "full" } else { "incremental" };
                let job = json_response(self.send(Method::POST, SCANS, Some(json!({"mode": mode})), None)?)?;
                Ok(LibraryUpdate::Scan(job))
            }
            LibraryOp::Status(id) => {
                let job = self.library_scans()?.into_iter().find(|job| job.id == id)
                    .context("Server did not return the requested scan status.")?;
                Ok(LibraryUpdate::Scan(job))
            }
            LibraryOp::Cancel(id) => {
                ensure!(!id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "Invalid library scan ID.");
                let response = self.send(Method::DELETE, &format!("{SCANS}/{id}"), None, None)?;
                match response.status() {
                    StatusCode::NO_CONTENT => {}
                    status if !status.is_success() => { let _: Value = json_response(response)?; }
                    _ => anyhow::bail!("Unexpected scan cancellation response."),
                }
                Ok(LibraryUpdate::CancelRequested)
            }
        }
    }

    fn library_scans(&self) -> Result<Vec<ScanJob>> {
        Ok(self.get::<ScanList>(SCANS)?.items)
    }
}
