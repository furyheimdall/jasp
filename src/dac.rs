use alsa::{Direction, ValueOr, pcm::{Access, Format, HwParams, PCM, State}};
use anyhow::{Context, Result, anyhow, ensure};
use reqwest::{Url, blocking::Client, header::{CONTENT_RANGE, HeaderValue, RANGE}};
use std::{io::{self, Read, Seek, SeekFrom}, path::Path, sync::{Arc, Mutex, mpsc::{self, Receiver, SyncSender}}, thread, time::{Duration, Instant}};
use symphonia::core::{
    audio::{AudioBuffer, AudioBufferRef, Signal}, codecs::{CODEC_TYPE_FLAC, CODEC_TYPE_MP3, Decoder, DecoderOptions},
    errors::Error as DecodeError, formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
    io::{MediaSource, MediaSourceStream}, meta::MetadataOptions, probe::Hint,
};
use symphonia::core::{conv::FromSample, sample::Sample};

pub(crate) mod ownership;
mod mpd_route;

const CHUNK: u64 = 256 * 1024;

pub fn devices() -> Vec<String> {
    scan_devices().unwrap_or_default()
}

fn scan_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();
    for entry in std::fs::read_dir("/sys/class/sound")?.flatten() {
        let name = entry.file_name();
        let Some((card, dev)) = name.to_str().and_then(|s| s.strip_prefix("pcmC"))
            .and_then(|s| s.strip_suffix('p')).and_then(|s| s.split_once('D')) else { continue };
        if card.parse::<u32>().is_err() || dev.parse::<u32>().is_err() { continue; }
        let card_path = Path::new("/sys/class/sound").join(format!("card{card}"));
        let Ok(device) = card_path.join("device").canonicalize() else { continue };
        if !device.ancestors().any(|p| p.join("subsystem").canonicalize().is_ok_and(|s| s == Path::new("/sys/bus/usb"))) { continue; }
        if let Ok(id) = std::fs::read_to_string(card_path.join("id")) {
            devices.push(format!("hw:CARD={},DEV={dev}", id.trim()));
        }
    }
    devices.sort_unstable();
    Ok(devices)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceCodec { Flac, Mp3 }

impl SourceCodec {
    pub const MIME_TYPES: [&str; 4] = ["audio/flac", "audio/x-flac", "audio/mpeg", "audio/mp3"];

    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime.split(';').next()?.trim() {
            "audio/flac" | "audio/x-flac" | "application/flac" => Some(Self::Flac),
            "audio/mpeg" | "audio/mp3" => Some(Self::Mp3),
            _ => None,
        }
    }

    pub fn from_track_format(format: &str) -> Option<Self> {
        if format.eq_ignore_ascii_case("flac") { Some(Self::Flac) }
        else if format.eq_ignore_ascii_case("mp3") { Some(Self::Mp3) }
        else { None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputFormat {
    pub codec: SourceCodec,
    pub rate: u32,
    pub channels: usize,
    pub source_bits: Option<u32>,
    pub pcm: Format,
}

pub struct Media {
    pub url: Url,
    pub cookie: HeaderValue,
    pub size: u64,
    pub codec: SourceCodec,
}

struct RangeSource {
    media: Media,
    http: Client,
    pos: u64,
    start: u64,
    buffer: Vec<u8>,
}

impl Read for RangeSource {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        if dst.is_empty() || self.pos >= self.media.size { return Ok(0); }
        if self.pos < self.start || self.pos - self.start >= self.buffer.len() as u64 {
            let end = (self.pos + CHUNK - 1).min(self.media.size - 1);
            let mut response = self.http.get(self.media.url.clone())
                .header("Cookie", self.media.cookie.clone())
                .header(RANGE, format!("bytes={}-{end}", self.pos))
                .send().map_err(|_| io::Error::other("media range request failed"))?;
            let expected = format!("bytes {}-{end}/{}", self.pos, self.media.size);
            if response.status() != 206 || response.headers().get(CONTENT_RANGE).and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
                return Err(io::Error::other("server refused the exact original byte range"));
            }
            let length = (end - self.pos + 1) as usize;
            self.buffer.resize(length, 0);
            if let Err(err) = response.read_exact(&mut self.buffer) {
                self.buffer.clear();
                return Err(err);
            }
            self.start = self.pos;
        }
        let source = &self.buffer[(self.pos - self.start) as usize..];
        let n = source.len().min(dst.len());
        dst[..n].copy_from_slice(&source[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeSource {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.pos.checked_add_signed(n),
            SeekFrom::End(n) => self.media.size.checked_add_signed(n),
        }.ok_or_else(|| io::Error::other("invalid media position"))?;
        Ok(self.pos)
    }
}

impl MediaSource for RangeSource {
    fn is_seekable(&self) -> bool { true }
    fn byte_len(&self) -> Option<u64> { Some(self.media.size) }
}

#[derive(Default, Clone)]
pub struct Status {
    pub active: bool,
    pub ended: bool,
    pub ms: u64,
    pub format: Option<OutputFormat>,
}

pub enum Action { Load(Media), Play, Pause, Seek(u64), Stop, Status }
type Request = (Action, SyncSender<Result<Status>>);

pub struct Output {
    tx: SyncSender<Request>,
    deadline: Arc<Mutex<Instant>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Output {
    pub fn start(device: String) -> Result<Self> {
        ensure!(devices().contains(&device), "selected USB DAC is no longer connected");
        let (tx, rx) = mpsc::sync_channel(1);
        let deadline = Arc::new(Mutex::new(Instant::now()));
        let playback_deadline = Arc::clone(&deadline);
        let thread = thread::Builder::new().name("jastreamer-alsa".into())
            .spawn(move || run(&device, rx, &playback_deadline))?;
        Ok(Self { tx, deadline, thread: Some(thread) })
    }

    pub fn renew(&self, deadline: Instant) {
        *self.deadline.lock().expect("deadline lock is not held during audio I/O") = deadline;
    }

    pub fn perform(&self, action: Action) -> Result<Status> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.try_send((action, tx)).context("USB audio worker unavailable")?;
        rx.recv_timeout(Duration::from_secs(4)).context("USB audio worker timed out")?
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let (unused, _) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.tx, unused));
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

fn run(device: &str, rx: Receiver<Request>, deadline: &Mutex<Instant>) {
    let mut stream: Option<Stream> = None;
    let mut failure = None;
    loop {
        let request = if stream.as_ref().is_some_and(|s| s.active) {
            match rx.recv_timeout(Duration::from_millis(4)) {
                Ok(request) => Some(request),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            }
        } else { match rx.recv() { Ok(request) => Some(request), Err(_) => return } };
        if let Some((action, reply)) = request {
            let result = match action {
                Action::Load(media) => {
                    stream = None;
                    failure = None;
                    Stream::load(media).map(|loaded| { stream = Some(loaded); })
                }
                Action::Stop => { stream = None; failure = None; Ok(()) }
                Action::Status => failure.take().map_or(Ok(()), Err),
                rest => stream.as_mut().context("no audio stream has been loaded").and_then(|s| {
                    check_lease(deadline)?;
                    match rest {
                        Action::Play => s.play(device, deadline),
                        Action::Pause => s.pause(),
                        Action::Seek(ms) => {
                            let was_active = s.active;
                            s.release();
                            s.seek(ms)?;
                            if was_active { s.play(device, deadline)?; }
                            Ok(())
                        }
                        _ => unreachable!(),
                    }
                }),
            };
            let _ = reply.send(result.map(|()| stream.as_ref().map_or_else(Status::default, Stream::status)));
        }
        if let Some(s) = stream.as_mut().filter(|s| s.active)
            && let Err(error) = check_lease(deadline).and_then(|()| s.pump(deadline)) {
            s.release();
            failure = Some(error);
        }
    }
}

fn check_lease(deadline: &Mutex<Instant>) -> Result<()> {
    ensure!(Instant::now() < *deadline.lock().expect("deadline lock is not held during audio I/O"),
        "jastreamer output lease expired");
    Ok(())
}

struct Stream {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    pcm: Option<(PCM, Format)>,
    rate: u32,
    channels: usize,
    codec: SourceCodec,
    source_bits: Option<u32>,
    track: u32,
    active: bool,
    ended: bool,
    tail: Tail,
    total_frames: Option<u64>,
    frame: u64,
    discard_before: u64,
    samples: Vec<u8>,
    offset: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tail { Reading, Decoded, Draining }

fn drain_pcm(tail: &mut Tail, start: impl FnOnce() -> alsa::Result<()>, state: impl FnOnce() -> State) -> Result<bool> {
    if *tail == Tail::Decoded {
        match start() {
            Ok(()) => return Ok(true),
            Err(error) if error.errno() == 11 => *tail = Tail::Draining,
            Err(error) => return Err(error).context("starting USB audio drain"),
        }
    }
    // The hw ioctl may return EAGAIN even in SETUP. Once requested, observe
    // completion via PCM state instead of calling nonblocking drain again.
    match state() {
        State::Setup => Ok(true),
        State::Draining => Ok(false),
        state => Err(anyhow!("USB audio drain ended in unexpected ALSA state {state:?}")),
    }
}

impl Stream {
    fn load(media: Media) -> Result<Self> {
        let codec = media.codec;
        let source = RangeSource { media, http: Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(2)).build()?,
            pos: 0, start: 0, buffer: Vec::new() };
        Self::open(codec, Box::new(source))
    }

    fn open(codec: SourceCodec, source: Box<dyn MediaSource>) -> Result<Self> {
        let mut hint = Hint::new();
        let (extension, decoder_type) = match codec {
            SourceCodec::Flac => ("flac", CODEC_TYPE_FLAC),
            SourceCodec::Mp3 => ("mp3", CODEC_TYPE_MP3),
        };
        hint.with_extension(extension);
        let reader = symphonia::default::get_probe().format(&hint, MediaSourceStream::new(source, Default::default()),
            &FormatOptions { enable_gapless: codec == SourceCodec::Mp3, ..FormatOptions::default() },
            &MetadataOptions::default())?.format;
        let track = reader.default_track().context("missing audio stream")?;
        let params = &track.codec_params;
        ensure!(params.codec == decoder_type, "audio stream does not match the advertised format");
        // MP3 frame counts can be estimated rather than declared in the source.
        let total_frames = if codec == SourceCodec::Flac { params.n_frames } else { None };
        let rate = params.sample_rate.filter(|n| *n > 0).context("missing audio sample rate")?;
        let source_bits = if codec == SourceCodec::Flac {
            let bits = params.bits_per_sample.context("missing FLAC bit depth")?;
            ensure!((4..=32).contains(&bits), "unsupported FLAC bit depth");
            Some(bits)
        } else { None };
        let channels = params.channels.context("missing audio channel count")?.count();
        ensure!(channels == 1 || channels == 2, "USB direct output supports mono and stereo");
        let id = track.id;
        let decoder = symphonia::default::get_codecs().make(params, &DecoderOptions::default())?;
        Ok(Self { reader, decoder, pcm: None, rate, channels, codec, source_bits, track: id, active: false,
            ended: false, tail: Tail::Reading, total_frames, frame: 0, discard_before: 0, samples: Vec::new(), offset: 0 })
    }

    fn play(&mut self, device: &str, deadline: &Mutex<Instant>) -> Result<()> {
        if self.active { return Ok(()); }
        ensure!(devices().iter().any(|d| d == device), "selected USB DAC was disconnected");
        let pcm = PCM::new(device, Direction::Playback, true).context("opening USB DAC (busy or unavailable)")?;
        let format = {
            let hw = HwParams::any(&pcm)?;
            hw.set_access(Access::RWInterleaved)?;
            hw.set_rate_resample(false)?;
            hw.set_channels(self.channels as u32).context("DAC does not support the exact source channel count")?;
            hw.set_rate(self.rate, ValueOr::Nearest).context("DAC does not support the source sample rate")?;
            let choices = if self.source_bits.is_some() {
                [Format::S16LE, Format::S243LE, Format::S24LE, Format::S32LE]
            } else {
                [Format::S32LE, Format::S243LE, Format::S24LE, Format::S16LE]
            };
            let format = choices.into_iter().find(|&format| self.source_bits.is_none_or(|bits| width(format) >= bits)
                && hw.test_format(format).is_ok())
                .context("DAC does not support a compatible PCM format")?;
            hw.set_format(format)?;
            hw.set_buffer_time_near(200_000, ValueOr::Nearest)?;
            hw.set_period_time_near(20_000, ValueOr::Nearest)?;
            pcm.hw_params(&hw)?;
            let actual = pcm.hw_params_current()?;
            ensure!(actual.get_rate()? == self.rate && actual.get_channels()? == self.channels as u32
                && actual.get_format()? == format, "DAC negotiated a different audio format");
            format
        };
        {
            let actual = pcm.hw_params_current()?;
            let sw = pcm.sw_params_current()?;
            sw.set_start_threshold(actual.get_buffer_size()? / 2)?;
            pcm.sw_params(&sw)?;
        }
        pcm.prepare()?;
        self.pcm = Some((pcm, format));
        self.active = true;
        // ALSA's PREPARED state is not playback. Report Play only after it
        // consumed enough PCM to run, or successfully drained a short track.
        let result = self.start_hardware(deadline);
        if result.is_err() { self.release(); }
        result
    }

    fn start_hardware(&mut self, deadline: &Mutex<Instant>) -> Result<()> {
        let timeout = Instant::now() + Duration::from_millis(1500);
        loop {
            check_lease(deadline)?;
            self.pump(deadline)?;
            if self.ended || self.pcm.as_ref().is_some_and(|(pcm, _)| {
                matches!(pcm.state(), State::Running | State::Draining)
            }) {
                return Ok(());
            }
            ensure!(Instant::now() < timeout, "USB DAC did not start consuming PCM");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn release(&mut self) {
        self.active = false;
        if let Some((pcm, _)) = self.pcm.take() { let _ = pcm.drop(); }
    }

    fn position(&self) -> u64 {
        let delay = self.pcm.as_ref().and_then(|(p, _)| p.delay().ok()).unwrap_or_default().max(0) as u64;
        self.frame.saturating_sub(delay)
    }

    fn pause(&mut self) -> Result<()> {
        if !self.active { return Ok(()); }
        let frame = self.position();
        self.release();
        self.seek_frame(frame)
    }

    fn seek(&mut self, ms: u64) -> Result<()> {
        self.seek_frame(ms.saturating_mul(self.rate.into()) / 1000)
    }

    fn seek_frame(&mut self, frame: u64) -> Result<()> {
        let seeked = self.reader.seek(SeekMode::Accurate, SeekTo::TimeStamp { ts: frame, track_id: self.track })?;
        self.decoder.reset();
        self.discard_before = seeked.required_ts;
        self.frame = frame;
        self.ended = false;
        self.tail = Tail::Reading;
        self.samples.clear();
        self.offset = 0;
        Ok(())
    }

    fn status(&self) -> Status {
        Status { active: self.active, ended: self.ended,
            ms: self.position().saturating_mul(1000) / u64::from(self.rate),
            format: self.pcm.as_ref().map(|(_, pcm)| OutputFormat { codec: self.codec, rate: self.rate,
                channels: self.channels, source_bits: self.source_bits, pcm: *pcm }), }
    }

    fn decode_chunk(&mut self, format: Format) -> Result<()> {
        self.samples.clear();
        self.offset = 0;
        let packet = match self.reader.next_packet() {
            Ok(packet) => packet,
            Err(DecodeError::IoError(err)) if err.kind() == io::ErrorKind::UnexpectedEof => {
                self.tail = Tail::Decoded;
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        let skip = self.discard_before.saturating_sub(packet.ts()) as usize;
        let audio = self.decoder.decode(&packet)?;
        ensure!(audio.spec().rate == self.rate && audio.spec().channels.count() == self.channels,
            "audio format changed midstream");
        match audio {
            AudioBufferRef::S32(decoded) if self.codec == SourceCodec::Flac =>
                interleave(&mut self.samples, &decoded, skip, self.channels, format, |sample| sample),
            AudioBufferRef::F32(decoded) if self.codec == SourceCodec::Mp3 =>
                interleave(&mut self.samples, &decoded, skip, self.channels, format, i32::from_sample),
            _ => return Err(anyhow!("unexpected decoder sample format")),
        }
        Ok(())
    }

    fn pump(&mut self, deadline: &Mutex<Instant>) -> Result<()> {
        if self.offset == self.samples.len() && self.tail == Tail::Reading {
            let format = self.pcm.as_ref().expect("playing owns PCM").1;
            self.decode_chunk(format)?;
        }
        let (pcm, _) = self.pcm.as_ref().expect("playing owns PCM");
        check_lease(deadline)?;
        if self.offset < self.samples.len() {
            let stride = pcm.frames_to_bytes(1) as usize;
            let data = &self.samples[self.offset..];
            match pcm.io_bytes().writei(data) {
                Ok(frames) => { self.offset += frames * stride; self.frame += frames as u64; }
                Err(e) if e.errno() == 11 => {}, // EAGAIN: the hardware buffer is full.
                Err(e) => return Err(e.into()),
            }
        } else if self.tail != Tail::Reading {
            if self.tail == Tail::Decoded {
                ensure!(self.total_frames.is_none_or(|total| self.frame == total), "FLAC length differs from STREAMINFO");
            }
            if drain_pcm(&mut self.tail, || pcm.drain(), || pcm.state())? {
                self.ended = true;
                self.release();
            }
        }
        Ok(())
    }
}

fn interleave<S: Sample>(out: &mut Vec<u8>, audio: &AudioBuffer<S>, skip: usize, channels: usize,
    format: Format, to_sample: impl Fn(S) -> i32) {
    for frame in skip..audio.frames() {
        for channel in 0..channels {
            append_sample(out, to_sample(audio.chan(channel)[frame]), format);
        }
    }
}

fn width(format: Format) -> u32 {
    match format { Format::S16LE => 16, Format::S24LE | Format::S243LE => 24, _ => 32 }
}

fn append_sample(out: &mut Vec<u8>, sample: i32, format: Format) {
    match format {
        Format::S16LE => out.extend_from_slice(&((sample >> 16) as i16).to_le_bytes()),
        Format::S243LE => out.extend_from_slice(&(sample >> 8).to_le_bytes()[..3]),
        Format::S24LE => out.extend_from_slice(&(sample >> 8).to_le_bytes()),
        _ => out.extend_from_slice(&sample.to_le_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_end_when_nonblocking_pcm_reaches_setup() {
        let mut tail = Tail::Decoded;
        assert!(!drain_pcm(&mut tail, || Err(alsa::Error::new("snd_pcm_drain", 11)), || State::Draining).unwrap());
        assert!(drain_pcm(&mut tail, || panic!("completed nonblocking drain must not restart"),
            || State::Setup).unwrap());
    }

    #[test]
    fn preserves_signed_sample_bits_in_hardware_containers() {
        for (sample, format, expected) in [
            (0x1234_0000, Format::S16LE, &[0x34, 0x12][..]),
            (0xfedc_ba00_u32 as i32, Format::S243LE, &[0xba, 0xdc, 0xfe]),
            (0xfedc_ba00_u32 as i32, Format::S24LE, &[0xba, 0xdc, 0xfe, 0xff]),
            (0x1234_5600, Format::S32LE, &[0x00, 0x56, 0x34, 0x12]),
        ] {
            let mut bytes = Vec::new();
            append_sample(&mut bytes, sample, format);
            assert_eq!(bytes, expected);
        }
    }

    #[test]
    fn decodes_original_stereo_mp3_and_flac_after_seeking() {
        use std::io::Cursor;

        let mp3 = include_bytes!("../tests/fixtures/stereo.mp3").as_slice();
        let flac = include_bytes!("../tests/fixtures/stereo.flac").as_slice();
        for (source, codec, source_bits, output) in [
            (mp3, SourceCodec::Mp3, None, Format::S32LE),
            (flac, SourceCodec::Flac, Some(16), Format::S16LE),
        ] {
            let mut stream = Stream::open(codec, Box::new(Cursor::new(source))).unwrap();
            assert_eq!((stream.rate, stream.channels, stream.source_bits), (44_100, 2, source_bits));
            stream.seek(150).unwrap();
            let mut bytes = 0;
            let mut audible = false;
            while stream.tail == Tail::Reading {
                stream.decode_chunk(output).unwrap();
                bytes += stream.samples.len();
                audible |= stream.samples.iter().any(|&byte| byte != 0);
            }
            let bytes_per_frame = if source_bits.is_some() { 4 } else { 8 };
            assert_eq!(bytes / bytes_per_frame, 8_820);
            assert!(audible);
        }
        assert!(Stream::open(SourceCodec::Flac, Box::new(Cursor::new(mp3))).is_err());
    }
}
