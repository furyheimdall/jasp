use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Receiver, SyncSender};

#[derive(Clone, Debug, Default, Serialize)]
pub struct Track {
    pub file: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub id: Option<u32>,
    pub position: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    #[default]
    Off,
    One,
    All,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TrackDetails {
    pub album_artist: String,
    pub date: String,
    pub genres: Vec<String>,
    pub disc: Option<u32>,
    pub number: Option<u32>,
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u32>,
    pub channels: Option<u32>,
    pub bit_rate: Option<u64>,
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub state: String,
    pub track: Track,
    pub track_details: TrackDetails,
    pub elapsed: f64,
    pub duration: f64,
    pub volume: Option<u8>,
    pub fixed_volume: bool,
    pub muted: bool,
    pub repeat: RepeatMode,
    pub random: bool,
    pub library_updating: bool,
    pub queue_length: usize,
    pub queue_version: u64,
    pub audio: String,
    pub source_audio: String,
    pub mpd_outputs: Vec<String>,
    pub usb_pcm: Vec<crate::dac::ownership::ObservedPcm>,
    pub local_usb_direct: bool,
    pub playback_error: Option<String>,
    pub source: Option<String>,
    pub can_volume: bool,
    pub can_transport: bool,
    pub capability_note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub path: String,
    pub label: String,
    pub kind: EntryKind,
    pub artist: String,
    pub album: String,
    pub duration: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Album {
    pub title: String,
    pub artist: String,
    pub cover_file: String,
    pub tracks: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum EntryKind {
    Directory,
    Track,
    Playlist,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePlacement {
    Play,
    Next,
    End,
}

#[derive(Clone, Debug)]
pub enum QueueSource {
    Track(String),
    Folder(String),
    Album(Vec<String>),
    Playlist(String),
}

#[derive(Clone, Debug)]
pub enum Command {
    Refresh,
    TogglePlayback,
    Play,
    Next,
    Previous,
    SetVolume(u8),
    SetRepeat(RepeatMode),
    SetRandom(bool),
    Seek(f64),
    PlayId(u32),
    LoadLibrarySettings { notify: bool },
    ScanLibrary { path: String, full: bool },
    PlayPath(String),
    QueueLibrary { placement: QueuePlacement, source: QueueSource },
    MoveQueueEntry { id: u32, earlier: bool, revision: u64 },
    RemoveQueueEntry { id: u32, revision: u64 },
    ClearQueue { revision: u64 },
    ListAlbums,
    ListPlaylists,
    Browse(String),
    Search(String),
    OpenPlaylist(String),
    LoadQueue,
    ReadOutputs,
    YieldUsb { request: u64, device: String },
}

pub enum Event {
    Snapshot(Snapshot),
    Queue(Vec<Track>),
    Outputs(Vec<String>),
    UsbYielded { request: u64, result: Result<(), String> },
    Albums(Vec<Album>),
    Entries { heading: String, entries: Vec<Entry> },
    LibrarySettings { notify: bool, result: Result<Vec<Entry>, String> },
    LibraryScanned(Result<Option<usize>, String>),
    Cover { key: String, rgba: Vec<u8>, width: usize, height: usize },
    CoverUnavailable(String),
    Thumbnail { key: String, rgba: Option<(Vec<u8>, usize, usize)> },
    LibraryError(String),
    Error(String),
    Disconnected(String),
}

pub struct Connection {
    pub server: String,
    pub read_only: bool,
    pub commands: SyncSender<Command>,
    pub artwork: SyncSender<String>,
    pub events: Receiver<Event>,
}
