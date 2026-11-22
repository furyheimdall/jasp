use crate::{chrome::{SignalPath, SignalQuality::{self, Converted, Lossy, Preserved, Unverified}}, model::Snapshot};

pub(super) struct MoodePath {
    source: String,
    decoder: &'static str,
    stream: String,
    output: String,
    hardware: String,
    note: &'static str,
    source_quality: SignalQuality,
    stream_quality: SignalQuality,
    device_quality: SignalQuality,
    flowing: bool,
}

impl MoodePath {
    pub(super) fn observed(snapshot: &Snapshot) -> Option<Self> {
        if snapshot.track.file.is_empty() || !matches!(snapshot.state.as_str(), "play" | "pause") { return None; }
        let suffix = snapshot.track.file.rsplit_once('.').map_or("", |(_, ext)| ext);
        let is = |extension: &str| suffix.eq_ignore_ascii_case(extension);
        let (decoder, source_quality) = if is("flac") { ("MPD · FLAC", Preserved) }
            else if is("alac") { ("MPD · ALAC", Preserved) }
            else if is("mp3") { ("MPD · MP3", Lossy) }
            else if is("aac") { ("MPD · AAC", Lossy) }
            else if is("opus") { ("MPD · Opus", Lossy) }
            else if is("wav") { ("MPD · WAV", Unverified) }
            else if is("aiff") || is("aif") { ("MPD · AIFF", Unverified) }
            else if is("dsf") || is("dff") { ("MPD · DSD", Unverified) }
            else { ("MPD", Unverified) };
        let source = format_report(&snapshot.source_audio);
        let stream = format_report(&snapshot.audio);
        let original = audio_parts(&snapshot.source_audio);
        let mpd = audio_parts(&snapshot.audio);
        let stream_changes = original.zip(mpd).map(|(source, output)| source.changes_to(output)).unwrap_or_default();
        let (output, hardware, hardware_changes, flowing) = match snapshot.usb_pcm.as_slice() {
            [pcm] => {
                let output = Pcm { rate: pcm.rate, channels: pcm.channels, bits: alsa_capacity(&pcm.format) };
                let measured = mpd.or(original).map(|input| input.changes_to(output)).unwrap_or_default();
                (pcm.device.clone(), format!("{} kHz · {} ch\n{}", f64::from(pcm.rate) / 1000.0, pcm.channels, pcm.format), measured, true)
            }
            multiple if !multiple.is_empty() => (
                "Multiple USB DACs".into(), "More than one active ALSA stream".into(),
                Changes::default(), false,
            ),
            _ => (
                snapshot.mpd_outputs.first().cloned().unwrap_or_else(|| "Output".into()),
                "Hardware format not observed".into(), Changes::default(), false,
            ),
        };
        let changes = stream_changes.merge(hardware_changes);
        let bit_perfect = source_quality == Preserved && snapshot.local_usb_direct
            && original.zip(mpd).is_some_and(|(source, stream)| source == stream
                && matches!(snapshot.usb_pcm.as_slice(), [pcm]
                    if source.rate == pcm.rate && source.channels == pcm.channels
                    && matches!((source.bits, alsa_capacity(&pcm.format)),
                        (Some(16), Some(16)) | (Some(24), Some(24 | 32)) | (Some(32), Some(32)))));
        let note = if bit_perfect { "Bit-perfect to USB · Based on configuration" }
        else if changes.narrowed { "PCM bit depth decreased" } else {
            match (changes.rate, changes.channels) {
                (true, true) => "Sample rate and channel count changed",
                (true, false) => "Sample rate changed",
                (false, true) => "Channel count changed",
                _ if source_quality == Lossy => "Lossy source",
                _ if flowing && original.is_some() => "Rate and channels match · Bit-perfect not verified",
                _ if flowing => "Hardware format observed · Source format unknown",
                _ => "Bit-perfect not verified",
            }
        };
        Some(Self { source, decoder, stream, output, hardware, note, source_quality,
            stream_quality: if bit_perfect { Preserved } else { stream_changes.quality() },
            device_quality: if bit_perfect { Preserved } else { hardware_changes.quality() }, flowing })
    }

    pub(super) fn display(&self) -> SignalPath<'_> {
        SignalPath {
            stages: [("moOde", &self.source, self.source_quality), ("Decoding", self.decoder, self.source_quality),
                ("MPD stream", &self.stream, self.stream_quality), (&self.output, &self.hardware, self.device_quality)],
            note: self.note,
            flowing: self.flowing,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Pcm { rate: u32, bits: Option<u32>, channels: u32 }

impl Pcm {
    fn changes_to(self, output: Self) -> Changes {
        Changes { rate: self.rate != output.rate, channels: self.channels != output.channels,
            narrowed: self.bits.zip(output.bits).is_some_and(|(a, b)| b < a) }
    }
}

#[derive(Clone, Copy, Default)]
struct Changes { rate: bool, channels: bool, narrowed: bool }

impl Changes {
    fn merge(self, other: Self) -> Self {
        Self { rate: self.rate || other.rate, channels: self.channels || other.channels, narrowed: self.narrowed || other.narrowed }
    }

    fn quality(&self) -> SignalQuality {
        if self.rate || self.channels || self.narrowed { Converted } else { Unverified }
    }
}

fn alsa_capacity(format: &str) -> Option<u32> {
    match format {
        "S8" | "U8" => Some(8),
        "S16_LE" | "S16_BE" => Some(16),
        "S20_LE" | "S20_BE" | "S20_3LE" | "S20_3BE" => Some(20),
        "S24_LE" | "S24_BE" | "S24_3LE" | "S24_3BE" => Some(24),
        "S32_LE" | "S32_BE" => Some(32),
        _ => None,
    }
}

fn audio_parts(value: &str) -> Option<Pcm> {
    let (rate, rest) = value.split_once(':')?;
    let (bits, channels) = rest.split_once(':')?;
    let rate = rate.parse().ok()?;
    let channels = channels.parse().ok()?;
    (rate > 0 && channels > 0).then_some(Pcm { rate, bits: bits.parse().ok(), channels })
}

fn format_report(value: &str) -> String {
    if let Some(Pcm { rate, bits: Some(bits), channels }) = audio_parts(value) {
        format!("{} kHz · {bits}-bit\n{channels} ch", f64::from(rate) / 1000.0)
    } else if value.is_empty() { "Format not reported".to_owned() }
    else { value.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn playing(file: &str, source: &str, audio: &str) -> Snapshot {
        Snapshot { state: "play".into(), track: Track { file: file.into(), ..Default::default() },
            source_audio: source.into(), audio: audio.into(), ..Default::default() }
    }

    #[test]
    fn colors_only_the_confirmed_conversion_when_hardware_is_unobserved() {
        let path = MoodePath::observed(&playing("stereo.FLAC", "96000:24:2", "48000:16:2")).unwrap();
        assert_eq!(path.display().stages.map(|(_, _, quality)| quality), [Preserved, Preserved, Converted, Unverified]);
        assert_eq!(path.display().note, "PCM bit depth decreased");
    }

    #[test]
    fn distinguishes_lossy_input_from_ambiguous_containers_and_unchanged_pcm() {
        let path = MoodePath::observed(&playing("song.mp3", "44100:16:2", "44100:16:2")).unwrap();
        assert_eq!(path.display().stages.map(|(_, _, quality)| quality), [Lossy, Lossy, Unverified, Unverified]);
        let path = MoodePath::observed(&playing("song.m4a", "44100:16:2", "44100:16:2")).unwrap();
        assert_eq!(path.display().stages.map(|(_, _, quality)| quality), [Unverified; 4]);
    }

    #[test]
    fn verified_direct_output_needs_the_same_pcm_and_preserves_24_bit_padding() {
        let mut snapshot = playing("stereo.flac", "96000:24:2", "96000:24:2");
        snapshot.usb_pcm.push(crate::dac::ownership::ObservedPcm {
            device: "K17".into(), rate: 96000, channels: 2, format: "S32_LE".into(), direct: true,
        });
        let current = |snapshot: &Snapshot| MoodePath::observed(snapshot).unwrap().note;
        assert_eq!(current(&snapshot), "Rate and channels match · Bit-perfect not verified");
        snapshot.local_usb_direct = true;
        let path = MoodePath::observed(&snapshot).unwrap();
        assert_eq!(path.display().stages.map(|(_, _, quality)| quality), [Preserved; 4]);
        assert_eq!(path.note, "Bit-perfect to USB · Based on configuration");
        snapshot.audio = "48000:24:2".into();
        assert_eq!(current(&snapshot), "Sample rate changed");
    }
}
