use super::{Album, Api, Event, Folder, Name, Queue, Track, json_response, track_details};
use crate::model::{QueuePlacement, TrackDetails};
use anyhow::{Context, Result, ensure};
use eframe::egui;
use reqwest::{Method, Url, blocking::{Client, Response}, header::{COOKIE, HeaderValue}};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use std::{
    collections::{HashSet, VecDeque},
    io::Read,
    sync::{Arc, Condvar, Mutex, Weak, mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError}},
    thread::{self, JoinHandle},
};

const QUEUE_LIMIT: usize = 10_000;
const COLLECTION_PAGE: usize = 200;
const THUMBNAIL_BACKLOG: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Collection {
    #[default]
    Albums,
    Artists,
    Genres,
    Folders,
    Tracks,
    MostPlayed,
    Liked,
    Playlists,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Scope {
    Album { id: String },
    Artist { name: String },
    Genre { name: String },
    Folder { root_id: String, path: String },
    Playlist { id: String },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Browse {
    pub collection: Collection,
    pub scope: Option<Scope>,
    pub query: String,
    pub offset: usize,
}

impl Browse {
    pub fn tracks(&self) -> bool {
        self.scope.is_some() || matches!(self.collection, Collection::Tracks | Collection::MostPlayed | Collection::Liked)
    }

    pub fn limit(&self) -> usize { if self.tracks() { 100 } else { 48 } }

    fn url(&self, base: &Url, offset: usize, limit: usize, recursive: bool) -> Result<Url> {
        let kind = if self.tracks() { "tracks" } else {
            match self.collection {
                Collection::Albums => "albums",
                Collection::Artists => "artists",
                Collection::Genres => "genres",
                Collection::Folders => "folders",
                Collection::Tracks | Collection::MostPlayed | Collection::Liked | Collection::Playlists => unreachable!(),
            }
        };
        let mut url = base.join("/api/v1/library/")?;
        url.path_segments_mut().expect("validated HTTP base").pop_if_empty().push(kind);
        let mut params = url.query_pairs_mut();
        params.append_pair("offset", &offset.to_string()).append_pair("limit", &limit.to_string());
        if !self.query.trim().is_empty() { params.append_pair("q", self.query.trim()); }
        if self.collection == Collection::Liked { params.append_pair("liked", "true"); }
        if self.collection == Collection::MostPlayed { params.append_pair("played", "true"); }
        let sort = match &self.scope {
            Some(Scope::Album { id }) => { params.append_pair("album_id", id); "album" }
            Some(Scope::Artist { name }) => { params.append_pair("artist", name); "album" }
            Some(Scope::Genre { name }) => { params.append_pair("genre", name); "album" }
            Some(Scope::Folder { root_id, path }) => {
                params.append_pair("root_id", root_id).append_pair("path", path);
                if recursive { params.append_pair("recursive", "true"); }
                "path"
            }
            Some(Scope::Playlist { .. }) => unreachable!("playlists use their own API"),
            None => match self.collection {
                Collection::Albums => "album",
                Collection::Artists => "artist",
                Collection::Genres => "genre",
                Collection::Folders => "path",
                Collection::Tracks | Collection::Liked => "title",
                Collection::MostPlayed => "most_played",
                Collection::Playlists => unreachable!(),
            },
        };
        params.append_pair("sort", sort);
        drop(params);
        Ok(url)
    }
}

#[derive(Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub track_ids: Vec<String>,
}

fn matches_search(text: &str, needle: &str) -> bool {
    needle.is_empty() || text.to_lowercase().contains(needle)
}

fn playlist_url(base: &Url, id: &str) -> Result<Url> {
    ensure!(!matches!(id, "" | "." | ".."), "invalid playlist ID");
    let mut url = base.join("/api/v1/playlists")?;
    url.path_segments_mut().expect("validated HTTP base").push(id);
    Ok(url)
}

pub struct Catalog { pub items: Rows, pub total: usize, pub folders: Vec<Folder> }

pub enum Rows {
    Albums(Vec<Album>),
    Names(Vec<Name>),
    Folders(Vec<Folder>),
    Tracks(Vec<Track>),
    Playlists(Vec<Playlist>),
}

pub enum QueueTarget { Track(String), Scope(Browse) }

#[derive(Deserialize)]
struct Page<T> { items: Option<Vec<T>>, total: usize }

impl<T> Page<T> {
    fn into_catalog(self, rows: impl FnOnce(Vec<T>) -> Rows) -> Catalog {
        Catalog { items: rows(self.items.unwrap_or_default()), total: self.total, folders: Vec::new() }
    }

    fn next_offset(&self, offset: usize) -> Result<Option<usize>> {
        let count = self.items.as_ref().map_or(0, Vec::len);
        let next = offset + count;
        if next >= self.total { return Ok(None); }
        ensure!(count != 0, "jastreamer returned an incomplete catalog page");
        Ok(Some(next))
    }
}

#[derive(Deserialize)]
struct TrackMatch { id: String, available: bool }

#[derive(Deserialize)]
struct EntryIdentity { id: String }

#[derive(Deserialize)]
struct QueueRevision { revision: u64, entries: Vec<EntryIdentity> }

pub(super) struct PreparedQueue { before: QueueRevision, track_ids: Vec<String> }

#[derive(Deserialize)]
struct QueueVersion { revision: u64 }

#[derive(Clone)]
pub(super) struct ReadSession { base: Url, http: Client, cookie: HeaderValue }

impl ReadSession {
    fn get(&self, url: Url) -> Result<Response> {
        ensure!(url.origin() == self.base.origin(), "unexpected API origin");
        self.http.get(url).header(COOKIE, self.cookie.clone()).send()
            .map_err(|_| anyhow::anyhow!("jastreamer API request failed"))
    }

    fn json<T: DeserializeOwned>(&self, url: Url) -> Result<T> { json_response(self.get(url)?) }

    fn track_details(&self, id: &str) -> Result<TrackDetails> {
        self.json::<track_details::Info>(track_details::url(&self.base, id)?)?.into_details(id)
    }

    fn playlist_tracks(&self, id: &str, query: &str) -> Result<Vec<Track>> {
        #[derive(Deserialize)]
        struct Detail { tracks: Vec<Track> }
        let Detail { mut tracks } = self.json(playlist_url(&self.base, id)?)?;
        let needle = query.trim().to_lowercase();
        if !needle.is_empty() {
            tracks.retain(|track| [&track.title, &track.artist, &track.album].iter().any(|text| matches_search(text, &needle)));
        }
        Ok(tracks)
    }

    fn browse(&self, request: &Browse, current: impl Fn() -> bool) -> Result<Catalog> {
        if let Some(Scope::Playlist { id }) = &request.scope {
            let tracks = self.playlist_tracks(id, &request.query)?;
            let total = tracks.len();
            let rows = tracks.into_iter().skip(request.offset).take(request.limit()).collect();
            return Ok(Catalog { items: Rows::Tracks(rows), total, folders: Vec::new() });
        }
        if request.collection == Collection::Playlists {
            #[derive(Deserialize)]
            struct Listing { items: Vec<Playlist> }
            let Listing { mut items } = self.json(self.base.join("/api/v1/playlists")?)?;
            let needle = request.query.trim().to_lowercase();
            if !needle.is_empty() { items.retain(|playlist| matches_search(&playlist.name, &needle)); }
            let total = items.len();
            let rows = items.into_iter().skip(request.offset).take(request.limit()).collect();
            return Ok(Catalog { items: Rows::Playlists(rows), total, folders: Vec::new() });
        }
        let url = request.url(&self.base, request.offset, request.limit(), false)?;
        let mut result = if request.tracks() {
            self.json::<Page<Track>>(url)?.into_catalog(Rows::Tracks)
        } else {
            match request.collection {
                Collection::Albums => self.json::<Page<Album>>(url)?.into_catalog(Rows::Albums),
                Collection::Artists | Collection::Genres => self.json::<Page<Name>>(url)?.into_catalog(Rows::Names),
                Collection::Folders => self.json::<Page<Folder>>(url)?.into_catalog(Rows::Folders),
                Collection::Tracks | Collection::MostPlayed | Collection::Liked | Collection::Playlists => unreachable!(),
            }
        };
        if let Some(Scope::Folder { root_id, path }) = &request.scope {
            let mut offset = 0;
            loop {
                ensure!(current(), "catalog request superseded");
                let mut url = self.base.join("/api/v1/library/folders")?;
                url.query_pairs_mut().append_pair("root_id", root_id).append_pair("path", path)
                    .append_pair("sort", "path").append_pair("offset", &offset.to_string())
                    .append_pair("limit", &COLLECTION_PAGE.to_string());
                let page: Page<Folder> = self.json(url)?;
                let next = page.next_offset(offset)?;
                result.folders.extend(page.items.into_iter().flatten()
                    .filter(|child| child.root_id == *root_id && child.path != *path));
                match next { Some(next) => offset = next, None => break }
            }
        }
        Ok(result)
    }

    fn prepare(&self, target: QueueTarget, purpose: QueuePurpose, current: impl Fn() -> bool) -> Result<PreparedQueue> {
        let track_ids = match target {
            QueueTarget::Track(id) => vec![id],
            QueueTarget::Scope(Browse { scope: Some(Scope::Playlist { id }), query, .. }) =>
                self.playlist_tracks(&id, &query)?.into_iter().filter(|track| track.available).map(|track| track.id).collect(),
            QueueTarget::Scope(request) => {
                ensure!(request.tracks(), "choose a track collection or open a catalog group first");
                let mut ids = Vec::new();
                let mut offset = 0;
                let mut total = None;
                loop {
                    ensure!(current(), "catalog session changed");
                    let url = request.url(&self.base, offset, COLLECTION_PAGE, true)?;
                    let page: Page<TrackMatch> = self.json(url)?;
                    ensure!(total.replace(page.total).is_none_or(|previous| previous == page.total),
                        "catalog changed while collecting tracks; try again");
                    let next = page.next_offset(offset)?;
                    for track in page.items.into_iter().flatten().filter(|track| track.available) {
                        ensure!(ids.len() < QUEUE_LIMIT, "the selection exceeds the 10,000-entry queue limit");
                        ids.push(track.id);
                    }
                    match next { Some(next) => offset = next, None => break }
                }
                if offset != 0 {
                    let distinct: HashSet<&str> = ids.iter().map(String::as_str).collect();
                    ensure!(distinct.len() == ids.len(), "catalog order changed while collecting tracks; try again");
                }
                ids
            }
        };
        let first = track_ids.first().context("no playable tracks match this selection")?;
        if matches!(purpose, QueuePurpose::UsbPlay) {
            ensure!(current(), "catalog session changed");
            super::usb_handoff::check_usb_track(&self.json::<Track>(track_url(&self.base, first)?)?)?;
        }
        ensure!(current(), "catalog session changed");
        let before: QueueRevision = self.json(self.base.join("/api/v1/queue")?)?;
        ensure!(before.entries.len() + track_ids.len() <= QUEUE_LIMIT,
            "this selection would exceed the 10,000-entry queue limit");
        Ok(PreparedQueue { before, track_ids })
    }

    fn artwork(&self, id: &str, size: u32) -> Result<egui::ColorImage> {
        let mut url = self.base.join("/api/v1/artwork")?;
        url.path_segments_mut().expect("validated HTTP base").push(id);
        let response = self.get(url)?.error_for_status()?;
        let mut data = Vec::new();
        response.take(8 * 1024 * 1024).read_to_end(&mut data)?;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(96 * 1024 * 1024);
        reader.limits(limits);
        let rgba = reader.decode()?.thumbnail(size, size).into_rgba8();
        Ok(egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
    }
}

impl Api {
    pub(super) fn read_session(&self) -> Option<ReadSession> {
        Some(ReadSession { base: self.base.clone(), http: self.http.clone(), cookie: self.cookie.as_ref()?.clone() })
    }

    pub(super) fn catalog_like(&self, id: &str, liked: bool) -> Result<Event> {
        ensure!(self.cookie.is_some(), "sign in to change liked tracks");
        let mut url = self.base.join("/api/v1/library/tracks")?;
        url.path_segments_mut().expect("validated HTTP base").push(id).push("like");
        let updated: Track = json_response(self.send(Method::PUT, url.as_str(), Some(json!({"liked":liked})), None)?)?;
        Ok(Event::TrackLiked(updated.id, updated.liked))
    }

    pub(super) fn check_usb_catalog(&self, prepared: &PreparedQueue) -> Result<()> {
        let id = prepared.track_ids.first().context("no playable tracks match this selection")?;
        super::usb_handoff::check_usb_track(&self.get::<Track>(track_url(&self.base, id)?.as_str())?)?;
        let queue: QueueVersion = self.get("/api/v1/queue")?;
        ensure!(queue.revision == prepared.before.revision, "the server queue changed; try USB Play again");
        Ok(())
    }

    pub(super) fn catalog_queue(&mut self, placement: QueuePlacement, prepared: PreparedQueue, emit: &impl Fn(Event)) -> Result<Event> {
        let PreparedQueue { before, track_ids } = prepared;
        let action = match placement { QueuePlacement::Play | QueuePlacement::Next => "next", QueuePlacement::End => "append" };
        if matches!(placement, QueuePlacement::Play) { self.require_local_player()?; }
        let queue: Queue = json_response(self.send(Method::POST, "/api/v1/queue",
            Some(json!({"action":action,"revision":before.revision,"track_ids":track_ids})), None)?)?;
        let play = if matches!(placement, QueuePlacement::Play) {
            (|| -> Result<()> {
                let entry = first_inserted_entry(&before, &queue, &track_ids)?;
                self.player_command("play", Some(entry), None)
            })().context("tracks were queued, but playback could not be requested")
        } else { Ok(()) };
        self.publish_queue(queue, emit);
        play?;
        Ok(Event::CatalogChanged(match placement {
            QueuePlacement::Play => "Playback requested",
            QueuePlacement::Next => "Added to play next",
            QueuePlacement::End => "Added to end of queue",
        }.into()))
    }

    pub(super) fn edit_queue(&mut self, revision: u64, entry: &str, index: Option<usize>, emit: &impl Fn(Event)) -> Result<Event> {
        ensure!(self.cookie.is_some(), "sign in to change the queue");
        let (body, notice) = match index {
            Some(index) => (json!({"action":"move","revision":revision,"entry_id":entry,"index":index}), "Queue entry moved"),
            None => (json!({"action":"remove","revision":revision,"entry_id":entry}), "Queue entry removed"),
        };
        self.mutate_queue(body, notice, emit)
    }

    pub(super) fn clear_queue(&mut self, revision: u64, emit: &impl Fn(Event)) -> Result<Event> {
        ensure!(self.cookie.is_some(), "sign in to change the queue");
        self.mutate_queue(json!({"action":"clear","revision":revision}), "Queue cleared", emit)
    }

    pub(super) fn shuffle_liked(&mut self, revision: u64, emit: &impl Fn(Event)) -> Result<Event> {
        ensure!(self.cookie.is_some(), "sign in to change the queue");
        self.mutate_queue(json!({"action":"append_liked_shuffled","revision":revision}), "Shuffled liked tracks added to queue", emit)
    }

    fn mutate_queue(&mut self, body: serde_json::Value, notice: &'static str, emit: &impl Fn(Event)) -> Result<Event> {
        let queue: Queue = json_response(self.send(Method::POST, "/api/v1/queue", Some(body), None)?)?;
        self.publish_queue(queue, emit);
        Ok(Event::CatalogChanged(notice.into()))
    }

    fn publish_queue(&mut self, queue: Queue, emit: &impl Fn(Event)) {
        self.queue_revision = Some(queue.revision);
        emit(Event::Queue(queue));
    }
}

fn first_inserted_entry<'a>(before: &QueueRevision, after: &'a Queue, tracks: &[String]) -> Result<&'a str> {
    ensure!(before.revision.checked_add(1) == Some(after.revision), "queue changed after inserting tracks");
    let old_ids: HashSet<&str> = before.entries.iter().map(|entry| entry.id.as_str()).collect();
    let mut added = after.entries.iter().filter(|entry| !old_ids.contains(entry.id.as_str()));
    let first = added.next().context("new queue entries could not be identified")?;
    ensure!(std::iter::once(first).chain(added).map(|entry| entry.track.id.as_str()).eq(tracks.iter().map(String::as_str)),
        "new queue entries did not match the selected tracks");
    Ok(&first.id)
}

fn track_url(base: &Url, id: &str) -> Result<Url> {
    let mut url = base.join("/api/v1/library/tracks")?;
    url.path_segments_mut().expect("validated HTTP base").push(id);
    Ok(url)
}

pub(super) enum QueuePurpose { Edit, UsbPlay }

type QueueRequest = (QueueTarget, QueuePurpose, SyncSender<Result<PreparedQueue>>, Weak<()>);

pub(super) struct PendingQueue {
    result: Receiver<Result<PreparedQueue>>,
    // Dropping the pending request cancels catalog pagination at the next page boundary.
    _live: Arc<()>,
}

impl PendingQueue {
    pub(super) fn try_recv(&self) -> Result<Result<PreparedQueue>, TryRecvError> { self.result.try_recv() }
}

#[derive(Default)]
struct Work {
    session: Option<ReadSession>,
    generation: u64,
    closed: bool,
    browse: Option<Browse>,
    browse_serial: u64,
    track_details: Option<String>,
    track_details_serial: u64,
    queue: Option<QueueRequest>,
    cover: Option<String>,
    cover_pending: bool,
    thumbnails: VecDeque<String>,
}

pub(super) struct Shared { work: Mutex<Work>, wake: Condvar }

impl Shared {
    pub(super) fn try_thumbnail(&self, id: String) -> Result<(), TrySendError<String>> {
        let mut work = self.work.lock().unwrap();
        if work.closed || work.session.is_none() { return Err(TrySendError::Disconnected(id)); }
        if work.thumbnails.iter().any(|queued| *queued == id) { return Ok(()); }
        if work.thumbnails.len() == THUMBNAIL_BACKLOG { return Err(TrySendError::Full(id)); }
        work.thumbnails.push_back(id);
        self.wake.notify_one();
        Ok(())
    }
}

pub(super) struct Reader { pub(super) shared: Arc<Shared>, thread: Option<JoinHandle<()>> }

impl Reader {
    pub(super) fn start(events: Sender<Event>, ctx: egui::Context) -> Result<Self> {
        let shared = Arc::new(Shared { work: Mutex::new(Work::default()), wake: Condvar::new() });
        let reader = shared.clone();
        let thread = thread::Builder::new().name("jastreamer-catalog".into())
            .spawn(move || run_reads(&reader, &events, &ctx))?;
        Ok(Self { shared, thread: Some(thread) })
    }

    pub(super) fn set_session(&self, session: Option<ReadSession>) {
        let mut work = self.shared.work.lock().unwrap();
        *work = Work { session, generation: work.generation.wrapping_add(1), ..Work::default() };
    }

    pub(super) fn browse(&self, request: Browse) -> Result<(), Browse> {
        let mut work = self.shared.work.lock().unwrap();
        if work.session.is_none() { return Err(request); }
        work.browse_serial = work.browse_serial.wrapping_add(1);
        work.browse = Some(request);
        self.shared.wake.notify_one();
        Ok(())
    }

    pub(super) fn track_details(&self, id: String) -> Result<(), String> {
        let mut work = self.shared.work.lock().unwrap();
        if work.session.is_none() { return Err(id); }
        work.track_details_serial = work.track_details_serial.wrapping_add(1);
        work.track_details = Some(id);
        self.shared.wake.notify_one();
        Ok(())
    }

    pub(super) fn prepare(&self, target: QueueTarget, purpose: QueuePurpose) -> Result<PendingQueue> {
        let mut work = self.shared.work.lock().unwrap();
        ensure!(work.session.is_some(), "sign in to change the queue");
        ensure!(work.queue.as_ref().is_none_or(|(_, _, _, live)| live.strong_count() == 0),
            "another catalog queue request is pending");
        let (reply, result) = mpsc::sync_channel(1);
        let live = Arc::new(());
        work.queue = Some((target, purpose, reply, Arc::downgrade(&live)));
        self.shared.wake.notify_one();
        Ok(PendingQueue { result, _live: live })
    }

    pub(super) fn cover(&self, artwork: &str) {
        let mut work = self.shared.work.lock().unwrap();
        if work.cover.as_deref().unwrap_or("") != artwork {
            work.cover = (!artwork.is_empty()).then(|| artwork.to_owned());
            work.cover_pending = work.cover.is_some();
            self.shared.wake.notify_one();
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        *self.shared.work.lock().unwrap() = Work { closed: true, ..Work::default() };
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

enum Job { Browse(u64, Browse), TrackDetails(u64, String), Queue(QueueRequest), Cover(String), Thumbnail(String) }

impl Work {
    fn next_job(&mut self) -> Option<Job> {
        if self.cover_pending {
            self.cover_pending = false;
            return self.cover.clone().map(Job::Cover);
        }
        if let Some(queue) = self.queue.take() { return Some(Job::Queue(queue)); }
        if let Some(id) = self.track_details.take() { return Some(Job::TrackDetails(self.track_details_serial, id)); }
        if let Some(browse) = self.browse.take() { return Some(Job::Browse(self.browse_serial, browse)); }
        self.thumbnails.pop_front().map(Job::Thumbnail)
    }

    fn active(&self, generation: u64) -> bool { !self.closed && self.generation == generation }
}

fn run_reads(shared: &Shared, events: &Sender<Event>, ctx: &egui::Context) {
    loop {
        let (job, session, generation) = {
            let mut work = shared.work.lock().unwrap();
            loop {
                if work.closed { return; }
                if let Some(job) = work.next_job() {
                    let session = work.session.clone().expect("catalog jobs require authentication");
                    break (job, session, work.generation);
                }
                work = shared.wake.wait(work).unwrap();
            }
        };
        // Synchronize publication with session changes (and replacement browse/cover requests).
        // Network and image decoding never run while the lock is held.
        let publish = |event, valid: &dyn Fn(&Work) -> bool| {
            let work = shared.work.lock().unwrap();
            if work.active(generation) && valid(&work) {
                let _ = events.send(event);
                ctx.request_repaint();
            }
        };
        match job {
            Job::Browse(serial, request) => {
                let valid = |work: &Work| work.browse_serial == serial;
                let result = session.browse(&request, || {
                    let work = shared.work.lock().unwrap();
                    work.active(generation) && valid(&work)
                }).map_err(|error| format!("{error:#}"));
                publish(Event::Catalog(request, result), &valid);
            }
            Job::TrackDetails(serial, id) => {
                let result = session.track_details(&id).map_err(|error| format!("{error:#}"));
                publish(Event::TrackDetails(id, result), &|work| work.track_details_serial == serial);
            }
            Job::Queue((target, purpose, reply, live)) => {
                let current = || live.strong_count() != 0 && shared.work.lock().unwrap().active(generation);
                if current() {
                    let result = session.prepare(target, purpose, current);
                    if current() { let _ = reply.send(result); }
                }
            }
            Job::Cover(id) => {
                if let Ok(image) = session.artwork(&id, 640) {
                    let work = shared.work.lock().unwrap();
                    if work.active(generation) && work.cover.as_deref() == Some(id.as_str()) {
                        let _ = events.send(Event::Art(id, image));
                        ctx.request_repaint();
                    }
                }
            }
            Job::Thumbnail(id) => {
                let image = session.artwork(&id, 288).ok();
                publish(Event::Thumbnail(id, image), &|_| true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ja::Entry;

    #[test]
    fn recursive_selection_uses_the_folder_filter_and_starts_at_first_page() {
        let request = Browse {
            collection: Collection::Folders,
            scope: Some(Scope::Folder { root_id: "root+1".into(), path: "Live / 2003".into() }),
            query: " Café ".into(),
            offset: 300,
        };
        let base = Url::parse("https://example.test/").unwrap();
        let url = request.url(&base, 0, COLLECTION_PAGE, true).unwrap();
        assert_eq!(url.path(), "/api/v1/library/tracks");
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(params["root_id"], "root+1");
        assert_eq!(params["path"], "Live / 2003");
        assert_eq!(params["q"], "Café");
        assert_eq!(params["recursive"], "true");
        assert_eq!(params["sort"], "path");
        assert_eq!(params["offset"], "0");
    }

    #[test]
    fn play_resolves_the_inserted_entry_not_an_existing_copy_of_the_track() {
        let before = QueueRevision { revision: 8, entries: vec![EntryIdentity { id: "earlier".into() }] };
        let make_entry = |id: &str| Entry {
            id: id.into(), status: String::new(), track: Track { id: "same-track".into(), ..Track::default() },
        };
        let mut after = Queue { revision: 9, entries: vec![make_entry("earlier"), make_entry("inserted")] };
        let tracks = ["same-track".into()];
        assert_eq!(first_inserted_entry(&before, &after, &tracks).unwrap(), "inserted");
        after.revision = 10;
        assert!(first_inserted_entry(&before, &after, &tracks).is_err());
    }
}
