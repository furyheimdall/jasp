use crate::model::{Album, Command, Connection, Entry, EntryKind, Event, QueuePlacement, QueueSource, RepeatMode, Snapshot, Track, TrackDetails};
use crate::dac::ownership::{PlaybackPcm, verify_mpd_peer};
use anyhow::{Context, Result, bail, ensure};
use eframe::egui;
use reqwest::{Url, blocking};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, TcpStream, ToSocketAddrs},
    sync::mpsc::{self, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};

mod art;

const TIMEOUT: Duration = Duration::from_secs(4);
const POLL: Duration = Duration::from_secs(2);
const SEARCH_LIMIT: usize = 200;

pub struct Client {
    base: Url,
    api: Url,
    http: blocking::Client,
    mpd_port: u16,
    socket: Option<BufReader<TcpStream>>,
}

impl Client {
    pub fn new(mut base: Url, mpd_port: u16) -> Result<Self> {
        ensure!(matches!(base.scheme(), "http" | "https") && base.host_str().is_some(),
            "expected an HTTP(S) moOde base URL");
        ensure!(base.path() == "/" && base.query().is_none() && base.fragment().is_none()
            && base.username().is_empty() && base.password().is_none(), "use a host URL without a path, query or credentials");
        base.set_path("/");
        Ok(Self {
            api: base.join("command/")?, base, mpd_port, socket: None,
            http: blocking::Client::builder().timeout(TIMEOUT).no_proxy()
                .redirect(reqwest::redirect::Policy::none()).build()?,
        })
    }

    fn request(&self, command: &str) -> Result<Value> {
        self.request_checked(command, false)
    }

    fn request_checked(&self, command: &str, require_loopback: bool) -> Result<Value> {
        let response = self.http.get(self.api.clone()).query(&[("cmd", command)])
            .send().with_context(|| format!("moOde {command}"))?.error_for_status()?;
        if require_loopback {
            ensure!(response.remote_addr().is_some_and(|address| address.ip().to_canonical().is_loopback()),
                "moOde API connection is not local loopback");
        }
        let mut body = String::new();
        response.take(256 * 1024).read_to_string(&mut body)?;
        let value: Value = serde_json::from_str(&body)
            .with_context(|| format!("moOde {command}: {}", body.trim().chars().take(160).collect::<String>()))?;
        for key in ["alert", "error"] {
            if let Some(message) = value[key].as_str() {
                bail!("{message}");
            }
        }
        if let Some(message) = value.as_object().and_then(|v| v.get("0")).and_then(Value::as_str)
            && message.starts_with("ACK") {
            bail!("{message}");
        }
        Ok(value)
    }

    fn fixed_volume(&self, volume: &Value, require_loopback: bool) -> bool {
        if let Some(scope) = volume["scope"].as_str() { return scope == "fixed"; }
        if require_loopback || volume["source"].as_str().is_some_and(|source| source != "mpd") { return false; }
        // Legacy get_volume reports the saved knob value even when Fixed disables it.
        let Ok(endpoint) = self.api.join("cfg-table.php") else { return false; };
        let Ok(response) = self.http.get(endpoint).query(&[("cmd", "get_cfg_system_value"), ("param", "mpdmixer")])
            .send().and_then(blocking::Response::error_for_status) else { return false; };
        let mut data = [0; 64];
        let mut body = response.take(data.len() as u64);
        let mut len = 0;
        while len < data.len() {
            match body.read(&mut data[len..]) {
                Ok(0) => break,
                Ok(read) => len += read,
                Err(_) => return false,
            }
        }
        matches!(serde_json::from_slice::<&str>(&data[..len]), Ok("none"))
    }

    fn connect(&self) -> Result<BufReader<TcpStream>> {
        let mut reader = BufReader::new(open_mpd(&self.base, self.mpd_port)?);
        mpd_greeting(&mut reader)?;
        Ok(reader)
    }

    fn mpd(&mut self, command: &str) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        self.mpd_each_line(command, |line| lines.push(std::mem::take(line)))?;
        Ok(lines)
    }

    fn mpd_each_line(&mut self, command: &str, mut receive: impl FnMut(&mut String)) -> Result<()> {
        let mut reader = match self.socket.take() {
            Some(reader) => reader,
            None => self.connect()?,
        };
        writeln!(reader.get_mut(), "{command}").context("sending MPD command")?;
        let mut size = 0;
        let mut line = String::new();
        loop {
            let n = reader.read_line(&mut line).context("reading MPD reply")?;
            ensure!(n > 0, "MPD disconnected");
            size += n;
            ensure!(size <= 32 * 1024 * 1024, "MPD reply exceeds 32 MiB");
            if line.ends_with('\n') { line.pop(); }
            if line == "OK" {
                self.socket = Some(reader);
                return Ok(());
            }
            if line.starts_with("ACK ") {
                self.socket = Some(reader);
                bail!("{line}");
            }
            receive(&mut line);
            line.clear();
        }
    }

    fn snapshot(&mut self, require_loopback: bool) -> Result<Snapshot> {
        // A single MPD batch keeps the transport and current track together.
        let lines = self.mpd("command_list_ok_begin\nstatus\ncurrentsong\ncommand_list_end")?;
        let mut parts = lines.split(|line| line == "list_OK");
        let status = parts.next().unwrap_or_default();
        let song = parts.next().context("missing currentsong response")?;
        let volume = self.request_checked("get_volume", require_loopback).context("reading moOde volume")?;
        let fixed_volume = self.fixed_volume(&volume, require_loopback);
        let level = volume["volume"].as_str().and_then(|value| value.parse::<u8>().ok())
            .or_else(|| volume["volume"].as_u64().and_then(|n| n.try_into().ok()))
            .filter(|level| *level <= 100);
        let source = volume["source"].as_str().map(str::to_owned);
        let mut track = parse_track(song);
        let mut state = field(status, "state").context("missing MPD playback state")?.to_owned();
        let mut elapsed = number(status, "elapsed");
        let mut duration = number(status, "duration");
        let mut audio = field(status, "audio").unwrap_or_default().to_owned();
        let mut playback_error = field(status, "error").map(str::to_owned);
        if let Some(source) = source.as_deref().filter(|source| *source != "mpd") {
            // MPD does not describe playback or position of an external renderer.
            track = Track { title: source.to_owned(), ..Track::default() };
            state = "external".into();
            elapsed = 0.0;
            duration = 0.0;
            audio.clear();
            playback_error = None;
        }
        let (source_audio, track_details) = if state == "external" || track.file.is_empty() {
            (String::new(), TrackDetails::default())
        } else {
            let format = field(song, "Format").unwrap_or_default();
            (format.to_owned(), parse_track_details(song, status, format, &track.file))
        };
        let (mut mpd_outputs, mut usb_pcm) = (Vec::new(), Vec::new());
        if state == "play" && !require_loopback && let Ok(outputs) = self.mpd("outputs") {
            mpd_outputs = enabled_outputs(&outputs);
            if is_loopback(&self.base) && matches!(has_enabled_alsa(&outputs), Ok(true))
                && let Some(socket) = &self.socket {
                let mut enabled = enabled_output_records(&outputs);
                let direct_output = enabled.next().filter(|record| fixed_volume
                    && field(record, "plugin") == Some("alsa") && enabled.next().is_none())
                    .and_then(|record| field(record, "outputname"));
                usb_pcm = crate::dac::ownership::observed_mpd_usb_pcm(socket.get_ref(), direct_output);
            }
        }
        let local_usb_direct = matches!(usb_pcm.as_slice(), [pcm] if pcm.direct)
            && field(status, "xfade").is_none_or(|value| value.parse::<u32>() == Ok(0))
            && self.mpd("replay_gain_status").is_ok_and(|lines| field(&lines, "replay_gain_mode") == Some("off"));
        Ok(Snapshot {
            state, track, track_details, elapsed, duration, volume: if fixed_volume { None } else { level }, fixed_volume,
            muted: volume["muted"].as_str() == Some("yes"),
            repeat: repeat_mode(status),
            random: field(status, "random") == Some("1"),
            library_updating: field(status, "updating_db").is_some(),
            queue_length: field(status, "playlistlength").and_then(|n| n.parse().ok()).unwrap_or_default(),
            queue_version: field(status, "playlist").and_then(|n| n.parse().ok()).unwrap_or_default(),
            audio, playback_error, source,
            source_audio, mpd_outputs, usb_pcm, local_usb_direct,
            can_volume: !fixed_volume && volume["can_volume"].as_bool().unwrap_or(level.is_some()),
            can_transport: volume["can_transport"].as_bool().unwrap_or(true),
            capability_note: (volume["can_transport"].is_null()).then(|| "MPD playback · legacy moOde API".into()),
        })
    }

    fn yield_usb(&mut self, device: &str) -> Result<()> {
        ensure!(is_loopback(&self.base), "USB DAC transfer requires a loopback moOde server");
        let pcm = PlaybackPcm::for_device(device)?;
        let Some(owner) = pcm.mpd_owner()? else { return Ok(()) };

        let checks = self.mpd("command_list_ok_begin\noutputs\nlistpartitions\ncommand_list_end")
            .context("Checking selected MPD outputs and partitions")?;
        let mut parts = checks.split(|line| line == "list_OK");
        let enabled_alsa = has_enabled_alsa(parts.next().unwrap_or_default())?;
        let partitions = parts.next().context("MPD did not report its partitions")?;
        // The kernel reports the MPD process, not which of its partitions owns the PCM.
        ensure!(partitions.iter().filter(|line| line.starts_with("partition: ")).count() == 1,
            "Cannot identify the USB DAC's playback partition in the selected MPD");
        let snapshot = self.snapshot(true).context("Checking the current moOde playback source")?;
        ensure!(snapshot.source.as_deref().is_none_or(|source| source == "mpd") && snapshot.can_transport,
            "moOde did not confirm an MPD-controlled playback source (reported source: {:?})", snapshot.source);
        let stream = self.socket.as_ref().context("Selected MPD connection is unavailable")?.get_ref();
        verify_mpd_peer(stream, owner, enabled_alsa)?;

        let Some(current_owner) = pcm.mpd_owner()? else { return Ok(()) };
        ensure!(current_owner == owner, "USB DAC ownership changed before stopping MPD");
        ensure!(matches!(snapshot.state.as_str(), "play" | "pause"),
            "Selected MPD still holds the USB DAC but is not playing or paused (state: {:?})", snapshot.state);
        self.mpd("stop").context("Could not confirm that selected MPD stopped for USB DAC transfer")?;
        pcm.wait_released(owner).context("MPD was stopped, but the USB DAC could not be released")
    }

    fn queue(&mut self) -> Result<Vec<Track>> {
        let lines = self.mpd("playlistinfo")?;
        Ok(records(&lines).map(parse_track).collect())
    }

    fn queue_library(&mut self, placement: QueuePlacement, source: QueueSource) -> Result<()> {
        let (verb, path) = match source {
            QueueSource::Track(path) => return self.queue_tracks(placement, [path.as_str()]),
            QueueSource::Album(paths) => return self.queue_tracks(placement, paths.iter().map(String::as_str)),
            QueueSource::Folder(path) => ("listallinfo", path),
            QueueSource::Playlist(path) => ("listplaylistinfo", path),
        };
        let mut files = Vec::new();
        self.mpd_each_line(&format!("{verb} {}", quote(&path)?), |line| {
            if line.starts_with("file: ") { files.push(std::mem::take(line)); }
        }).with_context(|| format!("Reading tracks from {path}"))?;
        self.queue_tracks(placement, files.iter().filter_map(|line| line.strip_prefix("file: ")))
    }

    fn queue_tracks<'a>(&mut self, placement: QueuePlacement, tracks: impl IntoIterator<Item = &'a str>) -> Result<()> {
        let mut tracks = tracks.into_iter().peekable();
        ensure!(tracks.peek().is_some(), "No playable tracks in this selection");
        let current = if placement == QueuePlacement::End {
            None
        } else {
            field(&self.mpd("status")?, "song").map(str::parse::<usize>).transpose()
                .context("Invalid MPD current queue position")?
        };
        let play = placement == QueuePlacement::Play;
        let mut batch = String::from(if play { "command_list_ok_begin\n" } else { "command_list_begin\n" });
        for (offset, path) in tracks.enumerate() {
            ensure!(!path.is_empty(), "Empty MPD track path");
            batch.push_str("addid ");
            push_quoted(&mut batch, path)?;
            if let Some(current) = current {
                let position = current.checked_add(offset).and_then(|pos| pos.checked_add(1))
                    .context("MPD insertion position overflow")?;
                write!(batch, " {position}").expect("writing to a String cannot fail");
            }
            batch.push('\n');
        }
        batch.push_str("command_list_end");
        // MPD stops a command list at the first failure without rolling back earlier commands.
        let mut first = play;
        let mut first_id = None;
        self.mpd_each_line(&batch, |line| {
            if first {
                if line.as_str() == "list_OK" { first = false; }
                else if let Some(id) = line.strip_prefix("Id: ") { first_id = id.parse::<u32>().ok(); }
            }
        }).context("Queue insertion failed; some tracks may already have been added")?;
        if play {
            let id = first_id
                .context("Tracks were queued, but MPD did not return the first new queue ID")?;
            self.mpd(&format!("playid {id}")).context("Tracks were queued, but playback could not be requested")?;
        }
        Ok(())
    }

    fn move_queue_entry(&mut self, id: u32, earlier: bool, revision: u64) -> Result<()> {
        let lines = self.mpd(&format!("command_list_ok_begin\nstatus\nplaylistid {id}\ncommand_list_end"))?;
        let mut replies = lines.split(|line| line == "list_OK");
        let status = replies.next().unwrap_or_default();
        require_queue_revision(status, revision)?;
        protect_current_queue_entry(status, id)?;
        let entry = replies.next().context("Missing MPD queue entry response")?;
        let position = field(entry, "Pos").and_then(|pos| pos.parse::<u32>().ok())
            .context("MPD did not report the queue entry's position")?;
        let length = field(status, "playlistlength").and_then(|len| len.parse::<u32>().ok())
            .context("MPD did not report the queue length")?;
        ensure!(position < length, "MPD queue entry is outside the queue");
        let target = if earlier {
            position.checked_sub(1).context("This is already the first queue entry")?
        } else {
            ensure!(position + 1 < length, "This is already the last queue entry");
            position + 1
        };
        self.mpd(&format!("moveid {id} {target}"))?;
        Ok(())
    }

    fn browse(&mut self, path: &str) -> Result<Vec<Entry>> {
        let lines = self.mpd(&format!("lsinfo {}", quote(path)?))?;
        Ok(records(&lines).filter_map(|record| {
            let (kind, path) = record.first()?.split_once(": ")?;
            let kind = match kind {
                "directory" => EntryKind::Directory, "file" => EntryKind::Track,
                "playlist" => EntryKind::Playlist, _ => return None,
            };
            Some(entry(record, kind, path))
        }).collect())
    }

    fn playlists(&mut self) -> Result<Vec<Entry>> {
        let lines = self.mpd("listplaylists")?;
        Ok(records(&lines).filter_map(|record| {
            let path = record.first()?.strip_prefix("playlist: ")?;
            Some(entry(record, EntryKind::Playlist, path))
        }).collect())
    }

    fn albums(&mut self) -> Result<Vec<Album>> {
        let lines = self.mpd("listallinfo")?;
        Ok(parse_albums(&lines))
    }

    fn library_settings(&mut self) -> Result<Vec<Entry>> {
        let mut sources = self.browse("")?;
        sources.retain(|entry| entry.kind == EntryKind::Directory);
        Ok(sources)
    }

    fn scan_library(&mut self, path: &str, full: bool) -> Result<()> {
        let command = if full { "rescan" } else { "update" };
        if path.is_empty() { self.mpd(command)?; }
        else { self.mpd(&format!("{command} {}", quote(path)?))?; }
        Ok(())
    }

    fn tracks_from(&mut self, command: &str) -> Result<Vec<Entry>> {
        let lines = self.mpd(command)?;
        Ok(records(&lines).filter_map(|record| {
            let path = field(record, "file")?;
            Some(entry(record, EntryKind::Track, path))
        }).collect())
    }

    fn control(&mut self, command: Command) -> Result<()> {
        match command {
            Command::TogglePlayback => { self.request("toggle_play_pause")?; }
            Command::Play => {
                let status = self.mpd("status")?;
                let play = match field(&status, "state") {
                    Some("pause") => "pause 0",
                    Some("play" | "stop") => "play",
                    _ => bail!("MPD did not report a recognized playback state"),
                };
                self.mpd(play)?;
            }
            Command::Previous => { self.request("previous")?; }
            Command::Next => { self.request("next")?; }
            Command::SetVolume(value) => { self.request(&format!("set_volume {value}"))?; }
            Command::SetRepeat(mode) => { self.mpd(repeat_commands(mode))?; }
            Command::SetRandom(value) => { self.mpd(if value { "random 1" } else { "random 0" })?; }
            Command::Seek(seconds) => { self.mpd(&format!("seekcur {seconds:.3}"))?; }
            Command::PlayId(id) => { self.mpd(&format!("playid {id}"))?; }
            Command::PlayPath(path) => {
                ensure!(path.starts_with("RADIO/") && path.ends_with(".pls"), "PlayPath requires a moOde radio playlist");
                self.request(&format!("play_item {path}"))?;
            }
            Command::QueueLibrary { placement, source } => self.queue_library(placement, source)?,
            Command::MoveQueueEntry { id, earlier, revision } => self.move_queue_entry(id, earlier, revision)?,
            Command::RemoveQueueEntry { id, revision } => {
                let status = self.mpd("status")?;
                require_queue_revision(&status, revision)?;
                protect_current_queue_entry(&status, id)?;
                self.mpd(&format!("deleteid {id}"))?;
            }
            Command::ClearQueue { revision } => {
                require_queue_revision(&self.mpd("status")?, revision)?;
                self.mpd("clear")?;
            }
            _ => unreachable!("not a control command"),
        }
        Ok(())
    }

    pub fn probe(mut self) -> Result<()> {
        let state = self.snapshot(false)?;
        let queue = self.queue()?;
        let library = self.browse("")?;
        let cover = art::Source::new(&self).image(&state.track.file, art::COVER_SIDE)
            .ok().map(|(_, w, h)| [w, h]);
        println!("{}", serde_json::to_string_pretty(&json!({
            "status": state, "queue_items": queue.len(), "library_root": library,
            "cover_dimensions": cover,
        }))?);
        Ok(())
    }

    pub fn spawn(mut self, read_only: bool, ctx: egui::Context) -> Result<Connection> {
        let server = self.base.to_string();
        let (commands, rx) = mpsc::sync_channel(8);
        let (tx, events) = mpsc::channel();
        let (artwork, cover) = art::start(art::Source::new(&self), tx.clone(), ctx.clone())?;
        std::thread::Builder::new().name("moode-io".into()).spawn(move || {
            let mut next_poll = Instant::now();
            let mut queue_open = false;
            let mut queue_fingerprint = None;
            let mut scanning = false;
            loop {
                if Instant::now() >= next_poll {
                    match self.snapshot(false) {
                        Ok(snapshot) => {
                            let current_file = cover.changed_file(&snapshot.track.file);
                            let fingerprint = snapshot.queue_version;
                            let scan_complete = scanning && !snapshot.library_updating;
                            if !send(&tx, &ctx, Event::Snapshot(snapshot)) { return; }
                            if scan_complete {
                                scanning = false;
                                let total = self.mpd("stats").ok().and_then(|lines| field(&lines, "songs")?.parse().ok());
                                if !send(&tx, &ctx, Event::LibraryScanned(Ok(total))) { return; }
                            }
                            if let Some(file) = current_file { cover.update(file); }
                            if queue_open && queue_fingerprint != Some(fingerprint) {
                                match self.queue() {
                                    Ok(queue) => {
                                        queue_fingerprint = Some(fingerprint);
                                        if !send(&tx, &ctx, Event::Queue(queue)) { return; }
                                    }
                                    Err(err) => if !send(&tx, &ctx, Event::Error(format!("Queue: {err:#}"))) { return; },
                                }
                            }
                        }
                        Err(err) => if !send(&tx, &ctx, Event::Disconnected(format!("{err:#}"))) { return; },
                    }
                    next_poll = Instant::now() + POLL;
                }
                let command = match rx.recv_timeout(next_poll.saturating_duration_since(Instant::now())) {
                    Ok(command) => command,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                let result = match command {
                    Command::Refresh => { next_poll = Instant::now(); continue; }
                    Command::LoadQueue => { queue_open = true; queue_fingerprint = None; next_poll = Instant::now(); continue; }
                    Command::ReadOutputs => self.mpd("outputs")
                        .context("Reading MPD outputs")
                        .map(|lines| Event::Outputs(enabled_outputs(&lines))),
                    Command::YieldUsb { request, device } => {
                        let result = if read_only {
                            Err("USB DAC transfer is disabled in read-only mode".into())
                        } else {
                            let result = self.yield_usb(&device).map_err(|error| format!("{error:#}"));
                            next_poll = Instant::now();
                            result
                        };
                        Ok(Event::UsbYielded { request, result })
                    }
                    Command::LoadLibrarySettings { notify } => {
                        let result = self.library_settings().map_err(|error| format!("Reading MPD library settings: {error:#}"));
                        Ok(Event::LibrarySettings { notify, result })
                    }
                    Command::ScanLibrary { path, full } => {
                        let result = if read_only {
                            Err(anyhow::anyhow!("Library scans are disabled in read-only mode"))
                        } else if scanning {
                            Err(anyhow::anyhow!("A library scan is already running"))
                        } else { self.scan_library(&path, full) };
                        match result {
                            Ok(()) => { scanning = true; next_poll = Instant::now(); continue; }
                            Err(error) => Ok(Event::LibraryScanned(Err(format!("{error:#}")))),
                        }
                    }
                    Command::ListAlbums => Ok(self.albums().map(Event::Albums).unwrap_or_else(library_error)),
                    Command::ListPlaylists => Ok(self.playlists().map(|entries| Event::Entries {
                        heading: "Playlists".into(), entries,
                    }).unwrap_or_else(library_error)),
                    Command::Browse(path) => Ok(self.browse(&path).map(|entries| Event::Entries {
                        heading: if path.is_empty() { "Music sources".into() } else { path }, entries,
                    }).unwrap_or_else(library_error)),
                    Command::Search(query) => Ok(quote(&query).and_then(|quoted| self.tracks_from(&format!(
                        "search any {quoted} window 0:{SEARCH_LIMIT}"
                    ))).map(|entries| Event::Entries { heading: format!("Search: {query} · up to {SEARCH_LIMIT}"), entries })
                        .unwrap_or_else(library_error)),
                    Command::OpenPlaylist(path) => Ok(quote(&path).and_then(|quoted| self.tracks_from(&format!(
                        "listplaylistinfo {quoted}"
                    ))).map(|entries| Event::Entries { heading: path, entries }).unwrap_or_else(library_error)),
                    control => {
                        if read_only { continue; }
                        next_poll = Instant::now();
                        match self.control(control) {
                            Ok(()) => continue,
                            Err(error) => Err(error),
                        }
                    }
                };
                let event = result.unwrap_or_else(|err| Event::Error(format!("{err:#}")));
                if !send(&tx, &ctx, event) { return; }
            }
        })?;
        Ok(Connection { server, read_only, commands, artwork, events })
    }
}

pub(crate) fn is_loopback(base: &Url) -> bool {
    base.host_str().is_some_and(|host| host == "localhost" ||
        host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok_and(|ip| ip.to_canonical().is_loopback()))
}

fn open_mpd(base: &Url, port: u16) -> Result<TcpStream> {
    let host = base.host_str().expect("validated host");
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let mut last = None;
    for address in (host, port).to_socket_addrs()? {
        let stream = match TcpStream::connect_timeout(&address, TIMEOUT) {
            Ok(stream) => stream,
            Err(err) => { last = Some(err); continue; }
        };
        stream.set_read_timeout(Some(TIMEOUT))?;
        stream.set_write_timeout(Some(TIMEOUT))?;
        return Ok(stream);
    }
    Err(last.map_or_else(|| anyhow::anyhow!("no addresses found"), anyhow::Error::from))
        .with_context(|| format!("connecting to MPD at {host}:{port}"))
}

fn mpd_greeting(reader: &mut impl BufRead) -> Result<()> {
    let mut line = String::new();
    mpd_short_line(reader, &mut line)?;
    ensure!(line.starts_with("OK MPD "), "invalid greeting from MPD");
    Ok(())
}

fn mpd_short_line(reader: &mut impl BufRead, line: &mut String) -> Result<()> {
    line.clear();
    reader.take(1024).read_line(line)?;
    ensure!(line.ends_with('\n'), "missing or oversized MPD response line");
    line.pop();
    Ok(())
}

fn library_error(error: anyhow::Error) -> Event {
    Event::LibraryError(format!("{error:#}"))
}

fn send(tx: &Sender<Event>, ctx: &egui::Context, event: Event) -> bool {
    if tx.send(event).is_err() { return false; }
    ctx.request_repaint();
    true
}

fn quote(value: &str) -> Result<String> {
    let mut quoted = String::with_capacity(value.len() + 2);
    push_quoted(&mut quoted, value)?;
    Ok(quoted)
}

fn push_quoted(out: &mut String, value: &str) -> Result<()> {
    ensure!(!value.contains(['\r', '\n', '\0']), "unsupported control character in MPD argument");
    out.push('"');
    let mut start = 0;
    for (offset, _) in value.match_indices(['\\', '"']) {
        out.push_str(&value[start..offset]);
        out.push('\\');
        start = offset;
    }
    out.push_str(&value[start..]);
    out.push('"');
    Ok(())
}

fn field<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
    lines.iter().find_map(|line| {
        let (name, value) = line.split_once(": ")?;
        name.eq_ignore_ascii_case(key).then_some(value)
    })
}

fn repeat_mode(status: &[String]) -> RepeatMode {
    match (field(status, "repeat"), field(status, "single")) {
        (Some("1"), Some("1")) => RepeatMode::One,
        // An armed one-shot isn't a persistent repeat-one mode.
        (Some("1"), _) => RepeatMode::All,
        _ => RepeatMode::Off,
    }
}

fn repeat_commands(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "command_list_begin\nrepeat 0\nsingle 0\ncommand_list_end",
        RepeatMode::One => "command_list_begin\nrepeat 1\nsingle 1\ncommand_list_end",
        RepeatMode::All => "command_list_begin\nrepeat 1\nsingle 0\ncommand_list_end",
    }
}

fn require_queue_revision(status: &[String], revision: u64) -> Result<()> {
    let version = field(status, "playlist").and_then(|value| value.parse::<u64>().ok())
        .context("MPD did not report the queue revision")?;
    ensure!(version == revision, "MPD queue changed; review the updated queue and try again");
    Ok(())
}

fn protect_current_queue_entry(status: &[String], id: u32) -> Result<()> {
    match field(status, "state") {
        Some("stop") => Ok(()),
        Some("play" | "pause") => {
            let current = field(status, "songid").and_then(|value| value.parse::<u32>().ok())
                .context("MPD did not report the active queue entry's ID")?;
            ensure!(current != id, "Cannot move or remove the playing or paused queue entry");
            Ok(())
        }
        _ => bail!("MPD did not report a recognized playback state"),
    }
}

fn enabled_outputs(lines: &[String]) -> Vec<String> {
    enabled_output_records(lines)
        .filter_map(|record| field(record, "outputname").filter(|name| !name.trim().is_empty()))
        .map(str::to_owned)
        .collect()
}

fn enabled_output_records(lines: &[String]) -> impl Iterator<Item = &[String]> {
    lines.split(|line| line.starts_with("outputid: ")).skip(1)
        .filter(|record| field(record, "outputenabled") == Some("1"))
}

fn has_enabled_alsa(lines: &[String]) -> Result<bool> {
    let mut unknown_plugin = false;
    for record in enabled_output_records(lines) {
        match field(record, "plugin") {
            Some("alsa") => return Ok(true),
            None => unknown_plugin = true,
            _ => (),
        }
    }
    ensure!(unknown_plugin, "Selected MPD has no enabled ALSA output");
    // Older MPD may not report output plugins; only exact socket ownership
    // may authorize stopping it in this case.
    Ok(false)
}

fn number(lines: &[String], key: &str) -> f64 {
    field(lines, key).and_then(|v| v.parse::<f64>().ok())
        .filter(|x| x.is_finite() && *x >= 0.0).unwrap_or_default()
}

fn records(lines: &[String]) -> impl Iterator<Item = &[String]> {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start == lines.len() { return None; }
        let end = lines[start + 1..].iter().position(|line| line.starts_with("file: ")
            || line.starts_with("directory: ") || line.starts_with("playlist: "))
            .map_or(lines.len(), |n| start + 1 + n);
        let record = &lines[start..end];
        start = end;
        Some(record)
    })
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parse_albums(lines: &[String]) -> Vec<Album> {
    let mut albums: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for record in records(lines) {
        let Some(file) = record.first().and_then(|line| line.strip_prefix("file: ")) else { continue };
        let Some(album) = text_tag(record, "album") else { continue };
        let artist = text_tag(record, "albumartist")
            .or_else(|| text_tag(record, "artist")).unwrap_or_default();
        albums.entry((album, artist)).or_default().push((
            ordinal(record, "disc").unwrap_or(1),
            ordinal(record, "track").unwrap_or(u32::MAX),
            entry(record, EntryKind::Track, file),
        ));
    }
    albums.into_iter().map(|((title, artist), mut tracks)| {
        tracks.sort_unstable_by(|a, b| (a.0, a.1, &a.2.path).cmp(&(b.0, b.1, &b.2.path)));
        let cover_file = tracks.iter().map(|(_, _, track)| track.path.as_str())
            .find(|path| art::valid_path(path)).unwrap_or_default().to_owned();
        Album {
            title: title.to_owned(), artist: artist.to_owned(), cover_file,
            tracks: tracks.into_iter().map(|(_, _, track)| track).collect(),
        }
    }).collect()
}

fn text_tag<'a>(lines: &'a [String], key: &str) -> Option<&'a str> {
    text_tags(lines, key).next()
}

fn text_tags<'a, 'k>(lines: &'a [String], key: &'k str) -> impl Iterator<Item = &'a str> + use<'a, 'k> {
    lines.iter().filter_map(move |line| {
        let (name, value) = line.split_once(": ")?;
        let value = value.trim();
        (name.eq_ignore_ascii_case(key) && !value.is_empty()).then_some(value)
    })
}

fn ordinal(lines: &[String], key: &str) -> Option<u32> {
    text_tag(lines, key)?.split('/').next()?.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

fn parse_track(lines: &[String]) -> Track {
    let file = field(lines, "file").unwrap_or_default();
    Track {
        title: field(lines, "title").filter(|s| !s.is_empty()).or_else(|| field(lines, "name"))
            .unwrap_or_else(|| basename(file)).to_owned(),
        file: file.to_owned(), artist: field(lines, "artist").unwrap_or_default().to_owned(),
        album: field(lines, "album").unwrap_or_default().to_owned(),
        duration: number(lines, "duration"),
        id: field(lines, "id").and_then(|s| s.parse().ok()),
        position: field(lines, "pos").and_then(|s| s.parse().ok()),
    }
}

fn parse_track_details(song: &[String], status: &[String], format: &str, file: &str) -> TrackDetails {
    let positive = |value: &str| value.parse::<u32>().ok().filter(|n| *n > 0);
    let mut details = TrackDetails {
        album_artist: text_tag(song, "AlbumArtist").unwrap_or_default().to_owned(),
        date: text_tag(song, "Date").unwrap_or_default().to_owned(),
        genres: text_tags(song, "Genre").map(str::to_owned).collect(),
        disc: ordinal(song, "Disc"),
        number: ordinal(song, "Track"),
        path: file.to_owned(),
        ..TrackDetails::default()
    };
    if let Some((first, rest)) = format.split_once(':') {
        if let Some((bits, channels)) = rest.split_once(':') {
            details.sample_rate = positive(first);
            details.channels = positive(channels);
            match bits {
                "f" => details.bits_per_sample = Some(32),
                "dsd" => {
                    details.codec = "DSD".into();
                    details.bits_per_sample = Some(1);
                    // MPD expresses numeric DSD rates as bytes per second.
                    details.sample_rate = details.sample_rate.and_then(|rate| rate.checked_mul(8));
                }
                _ => details.bits_per_sample = positive(bits),
            }
        } else if let Some(factor) = first.strip_prefix("dsd").and_then(positive) {
            details.codec = "DSD".into();
            details.bits_per_sample = Some(1);
            details.sample_rate = factor.checked_mul(44_100);
            details.channels = positive(rest);
        }
    }
    if matches!(field(status, "state"), Some("play" | "pause"))
        && let Some(id) = field(song, "Id")
        && field(status, "songid") == Some(id)
    {
        // This is MPD's measured current bitrate (kb/s), not a PCM-rate calculation.
        details.bit_rate = field(status, "bitrate").and_then(|n| n.parse::<u64>().ok())
            .filter(|n| *n > 0).and_then(|n| n.checked_mul(1000));
    }
    details
}

fn entry(lines: &[String], kind: EntryKind, path: &str) -> Entry {
    Entry {
        kind, path: path.to_owned(),
        label: field(lines, "title").filter(|s| !s.is_empty()).unwrap_or_else(|| basename(path)).to_owned(),
        artist: field(lines, "artist").unwrap_or_default().to_owned(),
        album: text_tag(lines, "album").unwrap_or_default().to_owned(),
        duration: number(lines, "duration"),
    }
}

#[cfg(test)]
mod tests {
    use super::{RepeatMode, enabled_outputs, parse_track_details, quote, repeat_commands, repeat_mode};

    #[test]
    fn repeat_modes_map_to_persistent_options_without_playback_commands() {
        for (repeat, single, expected) in [
            ("0", "0", RepeatMode::Off), ("0", "1", RepeatMode::Off),
            ("1", "0", RepeatMode::All), ("1", "1", RepeatMode::One),
            ("1", "oneshot", RepeatMode::All),
        ] {
            assert_eq!(repeat_mode(&[format!("repeat: {repeat}"), format!("single: {single}")]), expected);
        }
        for (mode, repeat, single) in [(RepeatMode::Off, "repeat 0", "single 0"),
            (RepeatMode::All, "repeat 1", "single 0"), (RepeatMode::One, "repeat 1", "single 1")] {
            assert!(repeat_commands(mode).lines().eq(["command_list_begin", repeat, single, "command_list_end"]));
        }
    }

    #[test]
    fn track_details_use_song_format_and_only_bitrate_for_the_current_id() {
        let song = ["Id: 7", "AlbumArtist: Quartet", "Disc: 2/3", "Track: 7/12",
            "Date: 2007", "Genre: Jazz", "genre: Instrumental"].map(str::to_owned);
        let mut status = ["state: play", "songid: 7", "bitrate: 912", "audio: 48000:16:2"].map(str::to_owned);
        let details = parse_track_details(&song, &status, "96000:24:2", "album/track.flac");
        assert_eq!(details.album_artist, "Quartet");
        assert_eq!(details.genres, ["Jazz", "Instrumental"]);
        assert_eq!((details.disc, details.number), (Some(2), Some(7)));
        assert_eq!(details.date, "2007");
        assert_eq!((details.sample_rate, details.bits_per_sample, details.channels), (Some(96000), Some(24), Some(2)));
        assert_eq!(details.bit_rate, Some(912000));
        assert!(details.codec.is_empty());

        status[1] = "songid: 8".into();
        let details = parse_track_details(&song, &status, "", "album/track.flac");
        assert_eq!(details.bit_rate, None);
        assert_eq!(details.sample_rate, None);

        let details = parse_track_details(&[], &[], "dsd64:2", "album/track.dsf");
        assert_eq!(details.codec, "DSD");
        assert_eq!((details.sample_rate, details.bits_per_sample, details.channels), (Some(2_822_400), Some(1), Some(2)));
    }

    #[test]
    fn mpd_file_argument_escapes_delimiters_and_rejects_line_breaks() {
        assert_eq!(quote(r#"folder\"name".flac"#).unwrap(), r#""folder\\\"name\".flac""#);
        for invalid in ["a\nb", "a\rb", "a\0b"] {
            assert!(quote(invalid).is_err());
        }
    }

    #[test]
    fn reports_only_named_enabled_outputs() {
        let lines = [
            "outputid: 0", "outputname: USB DAC", "outputenabled: 1",
            "outputid: 1", "outputname: Network", "outputenabled: 0",
            "outputid: 2", "outputname: Incomplete",
            "outputid: 3", "outputenabled: 1",
            "outputid: 4", "outputname: ", "outputenabled: 1",
            "outputid: 5", "outputname: Headphones", "outputenabled: 1",
        ].map(str::to_owned);
        assert_eq!(enabled_outputs(&lines), ["USB DAC", "Headphones"]);
    }

    #[test]
    fn blank_outputs_are_empty() {
        assert!(enabled_outputs(&[]).is_empty());
        assert!(enabled_outputs(&[String::new()]).is_empty());
    }
}
