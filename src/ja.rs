use crate::dac;
use crate::model::{QueuePlacement, RepeatMode, TrackDetails};
use anyhow::{Context, Result, bail, ensure};
use eframe::egui;
use reqwest::{Method, Url, blocking::{Client, Response}, header::{COOKIE, HeaderValue, ORIGIN, SET_COOKIE}};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{io::Read, sync::{Arc, mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError}}, time::{Duration, Instant}};
use zeroize::Zeroizing;

mod catalog;
mod library_admin;
mod profile;
mod track_details;
mod usb_handoff;
pub use catalog::{Browse, Catalog, Collection, QueueTarget, Rows, Scope};
pub use library_admin::{LibraryAdmin, LibraryOp, LibraryUpdate, Root, ScanCounts, ScanJob};

const REGISTER: &str = "/api/v1/browser-output/registrations";
const OWNER: &str = "X-Jastreamer-Browser-Token";

pub(crate) fn saved_server() -> Result<Option<(String, bool)>> {
    Ok(profile::Store::for_user()?.load()?.map(|saved| (saved.server().to_owned(), saved.allow_http())))
}

pub(crate) fn server_url(url: &str) -> Result<Url> {
    let base = Url::parse(url.trim()).context("invalid jastreamer URL")?;
    ensure!(matches!(base.scheme(), "http" | "https") && base.host_str().is_some() && base.path() == "/"
        && base.username().is_empty() && base.password().is_none() && base.query().is_none() && base.fragment().is_none(),
        "use a jastreamer HTTP(S) origin without credentials or a path");
    Ok(base)
}

#[derive(Deserialize)]
struct Identity { product: String, protocol: u32, id: String, name: String, version: String }

#[derive(Deserialize, Default)]
pub struct Track {
    pub id: String, pub title: String, pub artist: String, pub album: String,
    pub artwork_id: String, pub available: bool, pub format: String,
    #[serde(default)] pub duration_ms: u64,
    #[serde(default)] pub disc: u32,
    #[serde(default)] pub track: u32,
    #[serde(default)] pub liked: bool,
    #[serde(default)] pub play_count: u64,
}

#[derive(Deserialize)]
pub struct Album { pub id: String, pub title: String, pub artist: String, pub artwork_id: String, pub track_count: usize }

#[derive(Deserialize)]
pub struct Name { pub name: String, pub track_count: usize }

#[derive(Deserialize)]
pub struct Folder { pub root_id: String, pub path: String, pub name: String, pub track_count: usize }

#[derive(Deserialize)]
pub struct PlayerState {
    pub revision: u64,
    pub state: String, pub renderer_id: String, pub pending_command: String, pub error: String,
    pub position_ms: u64, pub duration_ms: u64, pub track: Option<Track>,
    pub current_entry_id: String, pub capabilities: Capabilities,
    #[serde(default)] pub shuffle: bool,
    #[serde(default)] pub repeat_mode: RepeatMode,
}

#[derive(Deserialize)]
pub struct Capabilities { pub play: bool, pub pause: bool, pub stop: bool, pub seek: bool }
#[derive(Deserialize)]
pub struct Entry { pub id: String, pub track: Track, #[serde(default)] pub status: String }
#[derive(Deserialize)]
pub struct Queue { pub revision: u64, pub entries: Vec<Entry> }
#[derive(Deserialize)]
pub struct Renderer {
    pub id: String, pub name: String, pub online: bool,
}

pub enum UsbPlay { Player(Option<String>), Catalog(QueueTarget) }

pub enum Action {
    Connect(String), Login { username: String, password: Zeroizing<String>, allow_http: bool, remember: bool }, Logout,
    Player(&'static str, Option<String>, Option<u64>), Select(String),
    SetShuffle(bool), SetRepeat(RepeatMode),
    Fallback { renderer: String, expected: String, revision: u64 },
    StopForUsb { expected: String },
    Queue, QueueEdit { revision: u64, entry: String, index: Option<usize> },
    QueueClear { revision: u64 },
    Browse(Browse), LibraryArt(String), CatalogQueue(QueuePlacement, QueueTarget), Like(String, bool),
    TrackDetails(String),
    LibraryAdmin(LibraryOp),
    Attach(String), Release, ScanDac,
    PrepareUsbPlay { request: u64, device: String, play: UsbPlay },
    CommitUsbPlay { request: u64 },
    ReleaseUsbForMoode { request: u64 },
}

pub enum Event {
    Connected { url: String, label: String }, SignedIn(bool), Player(PlayerState), Queue(Queue), Renderers(Vec<Renderer>),
    Catalog(Browse, Result<Catalog, String>), Thumbnail(String, Option<egui::ColorImage>),
    TrackDetails(String, Result<TrackDetails, String>),
    CatalogChanged(String), TrackLiked(String, bool), CatalogMutationFailed(String),
    LibraryAdmin(Result<LibraryUpdate, String>),
    Art(String, egui::ColorImage), Devices(Vec<String>),
    Local(Option<(String, String)>), Format(Option<dac::OutputFormat>), Error(String),
    UsbPrepared { request: u64, result: Result<(), String> },
    UsbCommitted { request: u64, result: Result<(), String> },
    UsbReleased { request: u64, result: Result<(), String> },
}

pub struct Worker { pub tx: ActionSender, pub rx: Receiver<Event>, thread: Option<std::thread::JoinHandle<()>> }

pub struct ActionSender { control: SyncSender<Action>, artwork: Arc<catalog::Shared> }

impl ActionSender {
    pub fn try_send(&self, action: Action) -> Result<(), TrySendError<Action>> {
        match action {
            Action::LibraryArt(id) => self.artwork.try_thumbnail(id).map_err(|error| match error {
                TrySendError::Full(id) => TrySendError::Full(Action::LibraryArt(id)),
                TrySendError::Disconnected(id) => TrySendError::Disconnected(Action::LibraryArt(id)),
            }),
            action => self.control.try_send(action),
        }
    }
}

impl Worker {
    pub fn start(read_only: bool, ctx: egui::Context) -> Result<Self> {
        let (tx, actions) = mpsc::sync_channel(8);
        let (events, rx) = mpsc::channel();
        let reads = catalog::Reader::start(events.clone(), ctx.clone())?;
        let artwork = reads.shared.clone();
        let thread = std::thread::Builder::new().name("jastreamer-control".into()).spawn(move || {
            let store = profile::Store::for_user().ok();
            let mut client = None::<Api>;
            let mut pending_queue = None::<(QueuePlacement, catalog::PendingQueue)>;
            let mut usb_stage = None::<usb_handoff::Stage>;
            let mut next_snapshot = Instant::now();
            let mut next_output = next_snapshot;
            let emit = |event| { let _ = events.send(event); ctx.request_repaint(); };
            let mutation = |result: Result<Event>| emit(result.unwrap_or_else(|error|
                Event::CatalogMutationFailed(format!("{error:#}"))));
            let cancel_usb = |stage: &mut Option<usb_handoff::Stage>, message: &str| {
                if let Some(stage) = stage.take() && stage.awaiting_catalog() {
                    emit(Event::UsbPrepared { request: stage.request, result: Err(message.into()) });
                }
            };
            loop {
                let now = Instant::now();
                if let Some(api) = client.as_mut() {
                    if api.output.is_some() && now >= next_output {
                        if let Err(error) = api.output_tick(&emit) {
                            api.release(&emit);
                            emit(Event::Error(format!("Local output released: {error:#}")));
                        }
                        next_output = Instant::now() + Duration::from_millis(200);
                    }
                    if api.cookie.is_some() && now >= next_snapshot {
                        if let Err(error) = api.snapshot(&reads, store.as_ref(), &emit) { emit(Event::Error(format!("{error:#}"))); }
                        if api.cookie.is_none() {
                            pending_queue = None;
                            cancel_usb(&mut usb_stage, "jastreamer session expired");
                        }
                        next_snapshot = Instant::now() + Duration::from_secs(2);
                    }
                }
                if let Some(stage) = usb_stage.as_mut() {
                    let request = stage.request;
                    if stage.expired() {
                        cancel_usb(&mut usb_stage, "USB Play preparation expired; try again");
                    } else if let Some(result) = stage.poll_catalog(client.as_ref()) {
                        if result.is_err() { usb_stage = None; }
                        emit(Event::UsbPrepared { request, result: result.map_err(|error| format!("{error:#}")) });
                    }
                }
                if let Some((placement, pending)) = &pending_queue {
                    let ready = match pending.try_recv() {
                        Ok(result) => Some(result),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("catalog worker stopped"))),
                    };
                    if let Some(result) = ready {
                        let placement = *placement;
                        pending_queue = None;
                        mutation(result.and_then(|prepared| client.as_mut().context("catalog session changed")?
                            .catalog_queue(placement, prepared, &emit)));
                        next_snapshot = Instant::now();
                        continue;
                    }
                }
                // Poll for prepared queue work without waiting on catalog HTTP on this thread.
                let wait = if client.as_ref().is_some_and(|api| api.output.is_some()) {
                    next_output.min(next_snapshot).saturating_duration_since(Instant::now())
                } else if client.as_ref().is_some_and(|api| api.cookie.is_some()) {
                    next_snapshot.saturating_duration_since(Instant::now())
                } else { Duration::from_secs(5) };
                let wait = if pending_queue.is_some() { wait.min(Duration::from_millis(25)) } else { wait };
                let wait = usb_stage.as_ref().map_or(wait, |stage| stage.wait(wait));
                let action = match actions.recv_timeout(wait) {
                    Ok(action) => action, Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => { if let Some(api) = client.as_mut() { api.release(&emit); } return; }
                };
                if matches!(&action, Action::Connect(_) | Action::Login { .. } | Action::Logout) {
                    cancel_usb(&mut usb_stage, "jastreamer session changed");
                } else if matches!(&action, Action::Player(..) | Action::Select(_) | Action::Fallback { .. }
                    | Action::StopForUsb { .. } | Action::Attach(_) | Action::Release
                    | Action::CatalogQueue(..) | Action::QueueEdit { .. } | Action::QueueClear { .. }) {
                    cancel_usb(&mut usb_stage, "USB Play preparation was superseded by another playback or queue request");
                }
                let result = match action {
                    Action::PrepareUsbPlay { request, device, play } => {
                        cancel_usb(&mut usb_stage, "USB Play preparation was superseded");
                        let result = (|| {
                            ensure!(!read_only, "playback changes are disabled in read-only mode");
                            ensure!(pending_queue.is_none(), "another catalog queue request is pending");
                            let api = client.as_ref().context("connect to a jastreamer server first")?;
                            usb_handoff::Stage::begin(request, device, play, api, &reads)
                        })();
                        match result {
                            Ok(stage) => {
                                let ready = !stage.awaiting_catalog();
                                usb_stage = Some(stage);
                                if ready { emit(Event::UsbPrepared { request, result: Ok(()) }); }
                            }
                            Err(error) => emit(Event::UsbPrepared { request, result: Err(format!("{error:#}")) }),
                        }
                        continue;
                    }
                    Action::CommitUsbPlay { request } => {
                        let result = (|| {
                            ensure!(!read_only, "playback changes are disabled in read-only mode");
                            ensure!(usb_stage.as_ref().is_some_and(|stage| stage.request == request),
                                "USB Play preparation is no longer active; try again");
                            let stage = usb_stage.take().expect("matching USB Play request");
                            ensure!(!stage.expired(), "USB Play preparation expired; try again");
                            let api = client.as_mut().context("jastreamer session changed")?;
                            stage.commit(api, &emit)
                        })();
                        emit(Event::UsbCommitted { request, result: result.map_err(|error| format!("{error:#}")) });
                        Ok(())
                    }
                    Action::ReleaseUsbForMoode { request } => {
                        let result = if read_only {
                            Err("playback changes are disabled in read-only mode".into())
                        } else {
                            cancel_usb(&mut usb_stage, "USB Play preparation was superseded by DAC release");
                            if let Some(api) = client.as_mut() { api.release(&emit); }
                            Ok(())
                        };
                        emit(Event::UsbReleased { request, result });
                        Ok(())
                    }
                    Action::Connect(url) => {
                        reads.set_session(None);
                        pending_queue = None;
                        if let Some(api) = client.as_mut() { api.release(&emit); }
                        emit(Event::SignedIn(false));
                        client = None;
                        Api::connect(&url).and_then(|api| {
                            emit(Event::Connected { url, label: format!("{} · {}", api.identity.name, api.identity.version) });
                            let api = client.insert(api);
                            if let Some(store) = &store && api.restore(store)? {
                                reads.set_session(api.read_session());
                                emit(Event::SignedIn(true));
                            }
                            Ok(())
                        })
                    }
                    Action::Login { username, password, allow_http, remember } => {
                        client.as_mut().context("connect to a jastreamer server first").and_then(|api| {
                            api.login(&username, &password, allow_http)?;
                            pending_queue = None;
                            reads.set_session(api.read_session());
                            emit(Event::SignedIn(true));
                            if remember && let Err(error) = store.as_ref().context("No private user state directory is configured.")
                                .and_then(|store| api.save(store, allow_http)) {
                                emit(Event::Error(format!("Signed in, but could not remember this session: {error:#}")));
                            }
                            Ok(())
                        })
                    }
                    Action::Logout => {
                        reads.set_session(None);
                        pending_queue = None;
                        client.as_mut().context("connect to a jastreamer server first").and_then(|api| {
                            api.release(&emit);
                            let cleared = api.clear_saved(store.as_ref());
                            let result = api.send(Method::POST, "/api/v1/logout", None, None);
                            api.cookie = None;
                            api.queue_revision = None;
                            emit(Event::SignedIn(false));
                            cleared.context("could not clear saved sign-in")?;
                            let response = result?;
                            if response.status() != 401 { response.error_for_status()?; }
                            Ok(())
                        })
                    }
                    Action::Browse(request) => {
                        if let Err(request) = reads.browse(request) {
                            emit(Event::Catalog(request, Err("sign in to browse the catalog".into())));
                        }
                        continue;
                    }
                    Action::TrackDetails(id) => {
                        if let Err(id) = reads.track_details(id) {
                            emit(Event::TrackDetails(id, Err("sign in to read track details".into())));
                        }
                        continue;
                    }
                    Action::CatalogQueue(placement, target) => {
                        let result = (|| {
                            ensure!(!read_only, "queue changes are disabled in read-only mode");
                            ensure!(pending_queue.is_none(), "another catalog queue request is pending");
                            reads.prepare(target, catalog::QueuePurpose::Edit)
                        })();
                        match result {
                            Ok(pending) => pending_queue = Some((placement, pending)),
                            Err(error) => mutation(Err(error)),
                        }
                        continue;
                    }
                    Action::QueueEdit { revision, entry, index } => {
                        mutation((|| {
                            ensure!(!read_only, "queue changes are disabled in read-only mode");
                            client.as_mut().context("connect to a jastreamer server first")?
                                .edit_queue(revision, &entry, index, &emit)
                        })());
                        Ok(())
                    }
                    Action::QueueClear { revision } => {
                        mutation((|| {
                            ensure!(!read_only, "queue changes are disabled in read-only mode");
                            client.as_mut().context("connect to a jastreamer server first")?.clear_queue(revision, &emit)
                        })());
                        Ok(())
                    }
                    Action::Like(id, liked) => {
                        mutation((|| {
                            ensure!(!read_only, "like changes are disabled in read-only mode");
                            client.as_ref().context("connect to a jastreamer server first")?.catalog_like(&id, liked)
                        })());
                        Ok(())
                    }
                    Action::LibraryAdmin(op) => {
                        let result = client.as_ref().context("connect to a jastreamer server first")
                            .and_then(|api| api.library_admin(op, read_only));
                        emit(Event::LibraryAdmin(result.map_err(|error| format!("{error:#}"))));
                        Ok(())
                    }
                    Action::ScanDac => { emit(Event::Devices(dac::devices())); Ok(()) }
                    action => client.as_mut().context("connect to a jastreamer server first")
                        .and_then(|api| api.action(action, read_only, &reads, &emit)),
                };
                if let Err(error) = result { emit(Event::Error(format!("{error:#}"))); }
                next_snapshot = Instant::now();
            }
        })?;
        Ok(Self { tx: ActionSender { control: tx, artwork }, rx, thread: Some(thread) })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let (tx, _) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.tx.control, tx));
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

struct Api {
    base: Url, http: Client, identity: Identity, cookie: Option<HeaderValue>,
    output: Option<Local>, watching_queue: bool, queue_revision: Option<u64>,
    artwork_id: String,
}

fn json_response<T: DeserializeOwned>(response: Response) -> Result<T> {
    if !response.status().is_success() {
        let status = response.status();
        // Return the machine code, not an echoed password, token, URL or arbitrary HTML.
        let error: Option<Value> = serde_json::from_reader(response.take(8192)).ok();
        let code = error.as_ref().and_then(|v| v["error"]["code"].as_str())
            .filter(|s| s.len() <= 80 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        bail!("jastreamer HTTP {status}{}", code.map_or_else(String::new, |code| format!(" · {code}")));
    }
    Ok(serde_json::from_reader(response.take(8 * 1024 * 1024)).context("invalid jastreamer JSON")?)
}

impl Api {
    fn connect(url: &str) -> Result<Self> {
        let base = server_url(url)?;
        let http = Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3)).build()?;
        let identity: Identity = json_response(http.get(base.join("/api/v1/discovery")?).send()
            .map_err(|_| anyhow::anyhow!("jastreamer discovery request failed"))?)?;
        ensure!(identity.product == "jastreamer" && identity.protocol == 1 && !identity.id.is_empty(), "incompatible jastreamer server");
        Ok(Self { base, http, identity, cookie: None, output: None,
            watching_queue: false, queue_revision: None, artwork_id: String::new() })
    }

    fn send(&self, method: Method, path: &str, body: Option<Value>, token: Option<&HeaderValue>) -> Result<Response> {
        let url = self.base.join(path)?;
        ensure!(url.origin() == self.base.origin(), "unexpected API origin");
        let mut request = self.http.request(method.clone(), url);
        if let Some(cookie) = &self.cookie { request = request.header(COOKIE, cookie.clone()); }
        if let Some(token) = token { request = request.header(OWNER, token.clone()); }
        if method != Method::GET {
            request = request.header(ORIGIN, self.base.origin().ascii_serialization()).header("X-Jastreamer-Request", "web");
            if method != Method::DELETE { request = request.json(&body.unwrap_or_else(|| json!({}))); }
        }
        request.send().map_err(|_| anyhow::anyhow!("jastreamer API request failed"))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        json_response(self.send(Method::GET, path, None, None)?)
    }

    fn login(&mut self, username: &str, password: &str, allow_http: bool) -> Result<()> {
        ensure!(self.base.scheme() == "https" || allow_http, "HTTPS required for login; enable trusted-LAN HTTP explicitly");
        let response = self.send(Method::POST, "/api/v1/login", Some(json!({"username":username,"password":password})), None)?;
        let cookie = response.headers().get_all(SET_COOKIE).iter().filter_map(|header| header.to_str().ok())
            .filter_map(|header| header.split(';').next())
            .find(|pair| pair.strip_prefix("jastreamer_session=").is_some_and(|v| !v.is_empty()))
            .map(HeaderValue::from_str).transpose()?;
        let _: Value = json_response(response)?;
        let mut cookie = cookie.context("server did not issue a jastreamer session")?;
        cookie.set_sensitive(true);
        self.cookie = Some(cookie);
        self.artwork_id.clear();
        Ok(())
    }

    fn restore(&mut self, store: &profile::Store) -> Result<bool> {
        #[derive(Deserialize)]
        struct Session { authenticated: bool }
        let Some(saved) = store.load()? else { return Ok(false); };
        let Some(cookie) = saved.cookie(&self.base, &self.identity.id) else { return Ok(false); };
        self.cookie = Some(cookie);
        match self.get::<Session>("/api/v1/session") {
            Ok(Session { authenticated: true }) => Ok(true),
            Ok(Session { authenticated: false }) => {
                let cleared = self.clear_saved(Some(store));
                self.cookie = None;
                cleared.context("could not clear expired sign-in")?;
                bail!("Saved jastreamer session expired; sign in again.")
            }
            Err(error) => {
                self.cookie = None;
                Err(error.context("Could not verify the saved sign-in. Use Connect / Verify to try again."))
            }
        }
    }

    fn save(&self, store: &profile::Store, allow_http: bool) -> Result<()> {
        let cookie = self.cookie.as_ref().context("no jastreamer session to remember")?;
        store.save(&self.base, &self.identity.id, allow_http, cookie)
    }

    fn clear_saved(&self, store: Option<&profile::Store>) -> Result<()> {
        if let (Some(store), Some(cookie)) = (store, &self.cookie) {
            store.clear(&self.base, &self.identity.id, cookie)?;
        }
        Ok(())
    }

    fn snapshot(&mut self, reads: &catalog::Reader, store: Option<&profile::Store>, emit: &impl Fn(Event)) -> Result<()> {
        let response = self.send(Method::GET, "/api/v1/player", None, None)?;
        if response.status() == 401 {
            self.release(emit);
            let cleared = self.clear_saved(store);
            self.cookie = None;
            reads.set_session(None);
            emit(Event::SignedIn(false));
            cleared.context("session expired; could not clear saved sign-in")?;
            bail!("jastreamer session expired; sign in again");
        }
        self.publish_player(json_response(response)?, reads, emit);
        let renderers: PageItems<Renderer> = self.get("/api/v1/renderers")?;
        emit(Event::Renderers(renderers.items));
        if self.watching_queue {
            let queue: Queue = self.get("/api/v1/queue")?;
            if self.queue_revision != Some(queue.revision) {
                self.queue_revision = Some(queue.revision);
                emit(Event::Queue(queue));
            }
        }
        Ok(())
    }

    fn publish_player(&mut self, player: PlayerState, reads: &catalog::Reader, emit: &impl Fn(Event)) {
        let art = player.track.as_ref().map_or("", |track| &track.artwork_id);
        let changed_art = self.artwork_id != art;
        if changed_art {
            self.artwork_id.clear();
            self.artwork_id.push_str(art);
        }
        emit(Event::Player(player));
        if changed_art { reads.cover(&self.artwork_id); }
    }

    fn registered_usb_renderer(&self) -> Result<&str> {
        ensure!(self.cookie.is_some(), "sign in to control playback on the Pi USB DAC");
        self.output.as_ref().filter(|output| Instant::now() < output.deadline)
            .map(|output| output.renderer.as_str()).context("register this app's Pi USB DAC first")
    }

    fn fresh_usb_player(&self) -> Result<(&str, PlayerState)> {
        self.registered_usb_renderer()?;
        let player = self.get("/api/v1/player")?;
        // The lease can expire while the snapshot request is in flight.
        Ok((self.registered_usb_renderer()?, player))
    }

    fn require_local_player(&self) -> Result<()> {
        let (renderer, player) = self.fresh_usb_player()?;
        ensure!(player.renderer_id == renderer, "select this app's Pi USB DAC in Outputs before controlling playback");
        Ok(())
    }

    fn player_command(&self, action: &str, entry: Option<&str>, position: Option<u64>) -> Result<()> {
        self.require_local_player()?;
        self.post_player_command(action, entry, position)
    }

    fn post_player_command(&self, action: &str, entry: Option<&str>, position: Option<u64>) -> Result<()> {
        // The server does not accept a renderer precondition on this endpoint.
        let _: Value = json_response(self.send(Method::POST, "/api/v1/player",
            Some(json!({"action":action,"entry_id":entry.unwrap_or_default(),"position_ms":position.unwrap_or_default()})), None)?)?;
        Ok(())
    }

    fn stop_for_usb(&self, expected: &str) -> Result<()> {
        let (renderer, player) = self.fresh_usb_player()?;
        ensure!(!expected.is_empty() && expected != renderer && player.renderer_id == expected,
            "the selected server output changed; review Outputs before stopping playback");
        ensure!(player.pending_command.is_empty(), "a playback command is pending; wait before stopping the server output");
        if player.state != "stopped" {
            ensure!(player.capabilities.stop, "the selected server output cannot be stopped");
            self.post_player_command("stop", None, None)?;
        }
        Ok(())
    }

    fn select_usb(&self, renderer: &str) -> Result<()> {
        ensure!(self.registered_usb_renderer()? == renderer, "select only this app's registered Pi USB DAC");
        let _: PlayerState = json_response(self.send(Method::PUT, "/api/v1/player/output",
            Some(json!({"renderer_id":renderer})), None)?)?;
        Ok(())
    }

    fn fallback_usb(&self, renderer: &str, expected: &str, revision: u64) -> Result<()> {
        ensure!(self.registered_usb_renderer()? == renderer, "fallback requires this app's registered Pi USB DAC");
        let _: PlayerState = json_response(self.send(Method::POST, "/api/v1/player/output/fallback",
            Some(json!({"renderer_id":renderer,"expected_renderer_id":expected,"expected_revision":revision})), None)?)?;
        Ok(())
    }

    fn action(&mut self, action: Action, read_only: bool, reads: &catalog::Reader, emit: &impl Fn(Event)) -> Result<()> {
        match action {
            Action::Queue => { self.watching_queue = true; self.queue_revision = None; }
            Action::Release => self.release(emit),
            Action::Attach(device) if !read_only => {
                if let Err(error) = self.attach(device, emit) {
                    emit(Event::Local(None));
                    return Err(error);
                }
            }
            Action::Select(renderer) if !read_only => self.select_usb(&renderer)?,
            Action::Fallback { renderer, expected, revision } if !read_only => self.fallback_usb(&renderer, &expected, revision)?,
            Action::Player(action, entry, position) if !read_only => self.player_command(action, entry.as_deref(), position)?,
            Action::SetShuffle(shuffle) if !read_only => {
                self.update_mode(json!({"shuffle": shuffle}), reads, emit)?;
            }
            Action::SetRepeat(repeat) if !read_only => {
                self.update_mode(json!({"repeat_mode": repeat}), reads, emit)?;
            }
            Action::StopForUsb { expected } if !read_only => self.stop_for_usb(&expected)?,
            _ => bail!("playback changes are disabled in read-only mode"),
        }
        Ok(())
    }

    fn update_mode(&mut self, body: Value, reads: &catalog::Reader, emit: &impl Fn(Event)) -> Result<()> {
        ensure!(self.cookie.is_some(), "sign in to change playback mode");
        let player = json_response(self.send(Method::POST, "/api/v1/player/mode", Some(body), None)?)?;
        self.publish_player(player, reads, emit);
        Ok(())
    }

    fn attach(&mut self, device: String, emit: &impl Fn(Event)) -> Result<()> {
        ensure!(self.cookie.is_some(), "sign in to register a Pi USB DAC");
        ensure!(self.output.is_none(), "release the current USB output first");
        let output = dac::Output::start(device.clone())?;
        let response: Registration = json_response(self.send(Method::POST, REGISTER,
            Some(json!({"name":"Pi USB DAC · Native", "protocol_info":dac::SourceCodec::MIME_TYPES})), None)?)?;
        let id = response.registration_id.strip_prefix("browser:").context("invalid output registration ID")?;
        ensure!(!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && response.device.id == response.registration_id, "invalid output registration ID");
        let mut token = HeaderValue::from_str(&response.owner_token)?;
        token.set_sensitive(true);
        let renderer = response.device.id;
        let deadline = deadline(Instant::now(), response.lease_duration_ms)?;
        output.renew(deadline);
        self.output = Some(Local { output, token, path: format!("{REGISTER}/browser%3A{id}"), renderer: renderer.clone(),
            device: device.clone(),
            deadline, sequence: 0, play_id: String::new(), duration: 0, last_observation: Instant::now(), ended: false,
            format: None });
        emit(Event::Local(Some((renderer, device))));
        Ok(())
    }

    fn release(&mut self, emit: &impl Fn(Event)) {
        if let Some(local) = self.output.take() {
            let Local { output, path, token, .. } = local;
            drop(output);
            emit(Event::Local(None));
            emit(Event::Format(None));
            if let Err(error) = self.send(Method::DELETE, &path, None, Some(&token)).and_then(|response| {
                if response.status().is_success() || response.status() == 404 { Ok(()) }
                else { json_response::<Value>(response).map(|_| ()) }
            }) { emit(Event::Error(format!("DAC closed; server release could not be confirmed: {error}"))); }
        }
    }

    fn output_tick(&mut self, emit: &impl Fn(Event)) -> Result<()> {
        let mut local = self.output.take().expect("active local output");
        let result = self.tick(&mut local, emit);
        self.output = Some(local);
        result
    }

    fn tick(&self, local: &mut Local, emit: &impl Fn(Event)) -> Result<()> {
        let now = Instant::now();
        ensure!(now < local.deadline, "local output lease expired");
        let response: Poll = json_response(self.send(Method::GET, &format!("{}/commands", local.path), None, Some(&local.token))?)?;
        ensure!(Instant::now() < local.deadline, "local output lease expired during poll");
        local.deadline = deadline(now, response.lease_duration_ms)?;
        local.output.renew(local.deadline);
        if response.cancel_before_sequence >= local.sequence && !local.play_id.is_empty() {
            // A rejected or timed-out command must not keep audio alive.
            local.output.perform(dac::Action::Stop)?;
            local.play_id.clear();
            local.set_format(None, emit);
        }
        if let Some(command) = response.command.filter(|c| c.sequence > local.sequence && c.sequence > response.cancel_before_sequence) {
            let sequence = command.sequence;
            let report = match self.execute(local, command, emit) {
                Ok(observation) => json!({"sequence":sequence,"result":"succeeded","observation":observation}),
                Err(error) => {
                    let _ = local.output.perform(dac::Action::Stop);
                    local.play_id.clear();
                    local.set_format(None, emit);
                    emit(Event::Error(format!("USB DAC: {error:#}")));
                    json!({"sequence":sequence,"result":"failed","error_code":"action_failed"})
                }
            };
            self.report(local, report)?;
            local.sequence = sequence;
        }
        if !local.play_id.is_empty() && !local.ended && local.last_observation.elapsed() >= Duration::from_millis(950) {
            local.last_observation = Instant::now();
            let observation = match local.output.perform(dac::Action::Status) {
                Ok(status) => {
                    local.ended = status.ended;
                    local.set_format(status.format, emit);
                    local.observation(if status.ended { "ended" } else { "timeupdate" }, &status)
                }
                Err(error) => {
                    local.ended = true;
                    local.set_format(None, emit);
                    emit(Event::Error(format!("USB DAC: {error:#}")));
                    local.observation("error", &dac::Status::default())
                }
            };
            self.report(local, json!({"sequence":local.sequence,"observation":observation}))?;
        }
        Ok(())
    }

    fn report(&self, local: &Local, body: Value) -> Result<()> {
        let response = self.send(Method::POST, &format!("{}/reports", local.path), Some(body), Some(&local.token))?;
        if !response.status().is_success() { let _: Value = json_response(response)?; }
        Ok(())
    }

    fn execute(&self, local: &mut Local, command: OutputCommand, emit: &impl Fn(Event)) -> Result<Value> {
        let (action, event) = match command.action.as_str() {
            "set_uri" => {
                let resource = command.resource.context("missing media resource")?;
                ensure!(resource.transformed == Some(false) && resource.seekable && resource.size > 0,
                    "USB direct output requires an original, seekable stream");
                let codec = dac::SourceCodec::from_mime(&resource.mime).context("unsupported USB audio format")?;
                ensure!(!command.play_id.is_empty(), "missing playback ID");
                let url = self.base.join(&resource.url).context("invalid media grant")?;
                ensure!(url.origin() == self.base.origin() && url.username().is_empty() && url.password().is_none()
                    && url.fragment().is_none() && url.query().is_none() && url.path().strip_prefix("/media/")
                        .is_some_and(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')),
                    "untrusted media grant");
                local.play_id = command.play_id;
                local.duration = resource.duration_ms;
                (dac::Action::Load(dac::Media { url, size: resource.size, codec,
                    cookie: self.cookie.clone().context("missing authenticated media session")? }), "loaded")
            }
            action => {
                ensure!(!local.play_id.is_empty() && command.play_id == local.play_id, "stale local playback command");
                match action {
                    "play" => (dac::Action::Play, "playing"), "pause" => (dac::Action::Pause, "pause"),
                    "seek" => (dac::Action::Seek(command.position_ms), "seeked"), "stop" => (dac::Action::Stop, "stopped"),
                    _ => bail!("unsupported local output command"),
                }
            }
        };
        let status = local.output.perform(action)?;
        local.ended = false;
        local.last_observation = Instant::now();
        local.set_format(status.format, emit);
        let observation = local.observation(event, &status);
        if event == "stopped" { local.play_id.clear(); }
        Ok(observation)
    }
}

struct Local {
    output: dac::Output, path: String, token: HeaderValue, deadline: Instant, sequence: u64, renderer: String,
    device: String,
    play_id: String, duration: u64, last_observation: Instant, ended: bool, format: Option<dac::OutputFormat>,
}
impl Local {
    fn set_format(&mut self, format: Option<dac::OutputFormat>, emit: &impl Fn(Event)) {
        if self.format != format { self.format = format; emit(Event::Format(format)); }
    }
    fn observation(&self, event: &str, status: &dac::Status) -> Value {
        json!({"event":event,"play_id":self.play_id,"has_position":event != "error", "position_ms":status.ms,
            "duration_ms":self.duration,"state":if status.active {"playing"} else {"paused"}})
    }
}

fn deadline(start: Instant, milliseconds: u64) -> Result<Instant> {
    ensure!((1..=60_000).contains(&milliseconds), "invalid jastreamer output lease");
    Ok(start + Duration::from_millis(milliseconds))
}

#[derive(Deserialize)] struct PageItems<T> { items: Vec<T> }
#[derive(Deserialize)] struct Registration { registration_id: String, owner_token: String, lease_duration_ms: u64, device: Renderer }
#[derive(Deserialize)] struct Poll { lease_duration_ms: u64, cancel_before_sequence: u64, command: Option<OutputCommand> }
#[derive(Deserialize)] struct OutputCommand {
    sequence: u64, action: String, #[serde(default)] play_id: String, #[serde(default)] position_ms: u64,
    resource: Option<Resource>,
}
#[derive(Deserialize)] struct Resource { url: String, mime: String, size: u64, duration_ms: u64, seekable: bool, transformed: Option<bool> }
