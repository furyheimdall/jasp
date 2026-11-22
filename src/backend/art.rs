use super::{Client, TIMEOUT, mpd_greeting, mpd_short_line, open_mpd, quote, send};
use crate::model::Event;
use anyhow::{Context, Result, bail, ensure};
use eframe::egui;
use reqwest::{Url, blocking};
use std::{
    io::{self, BufRead, BufReader, Cursor, Read, Write},
    net::TcpStream,
    sync::{Arc, Mutex, mpsc::{self, Sender, SyncSender}},
    thread,
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHUNKS: usize = 2048;
const THUMBNAIL_BACKLOG: usize = 6;
const MPD_BINARY_LIMIT: usize = 256 * 1024;
const THUMBNAIL_PAUSE: Duration = Duration::from_millis(60);
const THUMBNAIL_SIDE: u32 = 192;
pub(super) const COVER_SIDE: u32 = 640;

type Pixels = (Vec<u8>, usize, usize);

pub(super) fn valid_path(file: &str) -> bool {
    !file.is_empty()
        && file.len() <= 4096
        && !file.bytes().any(|byte| byte.is_ascii_control() || byte == b'\\')
        && !file.split('/').next().unwrap_or_default().contains(':')
        && !file.split('/').any(|part| matches!(part, "" | "." | ".."))
}

pub(super) struct Source {
    base: Url,
    http: blocking::Client,
    mpd_port: u16,
    socket: Option<BufReader<TimedStream>>,
}

impl Source {
    pub(super) fn new(client: &Client) -> Self {
        Self {
            base: client.base.clone(), http: client.http.clone(), mpd_port: client.mpd_port,
            socket: None,
        }
    }

    pub(super) fn image(&mut self, file: &str, side: u32) -> Result<Pixels> {
        ensure!(valid_path(file), "art requires a relative MPD music file");
        if let Some(url) = self.cached_url(file, side)
            && let Ok(data) = self.http_bytes(url)
            && let Ok(image) = decode(data, side) {
            return Ok(image);
        }
        let quoted = quote(file)?;
        for command in ["albumart", "readpicture"] {
            if let Ok(Some(data)) = self.mpd_bytes(command, &quoted)
                && let Ok(image) = decode(data, side) {
                return Ok(image);
            }
        }
        decode(self.legacy_bytes(file)?, side)
    }

    fn cached_url(&self, file: &str, side: u32) -> Option<Url> {
        let (mut dir, _) = file.rsplit_once('/')?;
        if dir.ends_with(".cue") { dir = dir.rsplit_once('/')?.0; }
        // moOde uses MD5 only as the on-disk folder cache key.
        let hash = md5::compute(dir.as_bytes());
        let suffix = if side == THUMBNAIL_SIDE { "_sm.jpg" } else { ".jpg" };
        self.base.join(&format!("imagesw/thmcache/{hash:x}{suffix}")).ok()
    }

    fn mpd_bytes(&mut self, command: &str, file: &str) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + TIMEOUT;
        let mut reader = match self.socket.take() {
            Some(mut reader) => {
                reader.get_mut().deadline = deadline;
                reader
            }
            None => {
                let stream = open_mpd(&self.base, self.mpd_port)?;
                stream.set_nodelay(true)?;
                let mut reader = BufReader::new(TimedStream { stream, deadline });
                mpd_greeting(&mut reader)?;
                writeln!(reader.get_mut().stream, "binarylimit {MPD_BINARY_LIMIT}")?;
                let mut reply = String::new();
                mpd_short_line(&mut reader, &mut reply)?;
                ensure!(reply == "OK" || reply.starts_with("ACK "), "invalid MPD binary limit response");
                reader
            }
        };
        let mut data = Vec::new();
        let mut line = String::new();
        let mut expected = None;
        for _ in 0..MAX_CHUNKS {
            let link = reader.get_mut();
            link.stream.set_write_timeout(Some(link.remaining()?))?;
            writeln!(link.stream, "{command} {file} {}", data.len())?;
            let Some(total) = read_chunk(&mut reader, &mut line, &mut data, expected)? else {
                self.socket = Some(reader);
                return Ok(None);
            };
            if data.len() == total {
                self.socket = Some(reader);
                return Ok(Some(data));
            }
            expected = Some(total);
        }
        bail!("MPD art exceeds the chunk limit")
    }

    fn legacy_bytes(&self, file: &str) -> Result<Vec<u8>> {
        // The legacy PHP handler may unescape PATH_INFO. Don't forward pre-encoded bytes.
        ensure!(!file.as_bytes().windows(3).any(|part| part[0] == b'%'
            && part[1].is_ascii_hexdigit() && part[2].is_ascii_hexdigit()), "encoded bytes in legacy art path");
        let mut url = self.base.join("coverart.php")?;
        url.path_segments_mut().expect("validated HTTP base").extend(file.split('/'));
        self.http_bytes(url)
    }

    fn http_bytes(&self, url: Url) -> Result<Vec<u8>> {
        let mut response = self.http.get(url).send()?.error_for_status()?;
        let length = response.content_length().unwrap_or_default();
        ensure!(length <= MAX_BYTES as u64, "cover exceeds 8 MiB");
        let mut data = Vec::with_capacity(length as usize);
        (&mut response).take(MAX_BYTES as u64).read_to_end(&mut data)?;
        if data.len() == MAX_BYTES {
            ensure!(response.read(&mut [0])? == 0, "cover exceeds 8 MiB");
        }
        Ok(data)
    }
}

struct TimedStream {
    stream: TcpStream,
    deadline: Instant,
}

impl TimedStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline.checked_duration_since(Instant::now()).filter(|time| !time.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
    }
}

impl Read for TimedStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buf)
    }
}

fn read_chunk(
    reader: &mut impl BufRead,
    line: &mut String,
    data: &mut Vec<u8>,
    expected: Option<usize>,
) -> Result<Option<usize>> {
    mpd_short_line(reader, line)?;
    if line.starts_with("ACK ") || (*line == "OK" && expected.is_none()) { return Ok(None); }
    let total: usize = line.strip_prefix("size: ").context("missing MPD art size")?.parse()?;
    ensure!(total <= MAX_BYTES, "cover exceeds 8 MiB");
    ensure!(expected.is_none_or(|previous| total == previous), "MPD cover size changed between chunks");
    mpd_short_line(reader, line)?;
    if line.starts_with("type: ") { mpd_short_line(reader, line)?; }
    if total == 0 && *line == "OK" { return Ok(None); }
    let chunk: usize = line.strip_prefix("binary: ").context("missing MPD binary length")?.parse()?;
    let offset = data.len();
    ensure!(chunk > 0 && chunk <= total.saturating_sub(offset), "invalid MPD binary length");
    if offset == 0 { data.reserve_exact(total); }
    data.resize(offset + chunk, 0);
    reader.read_exact(&mut data[offset..])?;
    let mut end = [0];
    reader.read_exact(&mut end)?;
    ensure!(end == [b'\n'], "missing MPD binary delimiter");
    mpd_short_line(reader, line)?;
    ensure!(*line == "OK", "invalid MPD art completion");
    Ok(Some(total))
}

fn decode(data: Vec<u8>, side: u32) -> Result<Pixels> {
    let mut reader = image::ImageReader::new(Cursor::new(data)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.thumbnail(side, side).into_rgba8();
    let (width, height) = image.dimensions();
    Ok((image.into_raw(), width as usize, height as usize))
}

#[derive(Default)]
struct Current {
    file: String,
    pending: bool,
}

pub(super) struct CurrentCover {
    current: Arc<Mutex<Current>>,
    wake: SyncSender<String>,
}

impl CurrentCover {
    pub(super) fn changed_file(&self, file: &str) -> Option<String> {
        (self.current.lock().unwrap().file != file).then(|| file.to_owned())
    }

    pub(super) fn update(&self, file: String) {
        *self.current.lock().unwrap() = Current { pending: !file.is_empty(), file };
        // A full channel already makes the worker runnable. Empty keys only wake it.
        let _ = self.wake.try_send(String::new());
    }
}

pub(super) fn start(source: Source, tx: Sender<Event>, ctx: egui::Context)
    -> Result<(SyncSender<String>, CurrentCover)> {
    let (requests, rx) = mpsc::sync_channel::<String>(THUMBNAIL_BACKLOG);
    let current = Arc::new(Mutex::new(Current::default()));
    let work = current.clone();
    thread::Builder::new().name("moode-art".into()).spawn(move || {
        let mut source = source;
        let mut thumbnail: Option<String> = None;
        loop {
            let cover = {
                let mut state = work.lock().unwrap();
                std::mem::take(&mut state.pending).then(|| state.file.clone())
            };
            if let Some(key) = cover {
                let image = source.image(&key, COVER_SIDE).ok();
                let state = work.lock().unwrap();
                if state.file == key {
                    let event = match image {
                        Some((rgba, width, height)) => Event::Cover { key, rgba, width, height },
                        None => Event::CoverUnavailable(key),
                    };
                    if !send(&tx, &ctx, event) { return; }
                }
            } else if let Some(key) = thumbnail.take() {
                let rgba = source.image(&key, THUMBNAIL_SIDE).ok();
                if !send(&tx, &ctx, Event::Thumbnail { key, rgba }) { return; }
                thread::sleep(THUMBNAIL_PAUSE);
            } else {
                match rx.recv() {
                    Ok(key) if !key.is_empty() => thumbnail = Some(key),
                    Ok(_) => (),
                    Err(_) => return,
                }
            }
        }
    })?;
    Ok((requests.clone(), CurrentCover { current, wake: requests }))
}

#[cfg(test)]
mod tests {
    use super::read_chunk;

    #[test]
    fn binary_payloads_are_counted_not_read_as_lines() {
        let mut reply = &b"size: 7\ntype: image/jpeg\nbinary: 4\n\x00\nOK\nOK\nsize: 7\nbinary: 3\n\xff\r\n\nOK\n"[..];
        let mut line = String::new();
        let mut data = Vec::new();
        let size = read_chunk(&mut reply, &mut line, &mut data, None).unwrap();
        assert_eq!(size, Some(7));
        assert_eq!(read_chunk(&mut reply, &mut line, &mut data, size).unwrap(), size);
        assert_eq!(data, b"\x00\nOK\xff\r\n");
        assert!(reply.is_empty());
    }
}
