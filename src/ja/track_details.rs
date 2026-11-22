use crate::model::TrackDetails;
use anyhow::{Result, ensure};
use reqwest::Url;
use serde::Deserialize;

pub(super) fn url(base: &Url, id: &str) -> Result<Url> {
    // The URL crate omits dot segments, which would no longer name this track.
    ensure!(!matches!(id, "" | "." | ".."), "invalid track ID");
    let mut url = base.join("/api/v1/library/tracks")?;
    url.path_segments_mut().expect("validated HTTP base").push(id).push("info");
    Ok(url)
}

#[derive(Deserialize)]
pub(super) struct Info {
    track: CatalogTrack,
    audio: Option<Audio>,
    tags: Option<Tags>,
}

#[derive(Deserialize)]
struct CatalogTrack {
    id: String,
    #[serde(default)] album_artist: String,
    genres: Option<Vec<String>>,
    disc: Option<u32>,
    track: Option<u32>,
    play_count: Option<u64>,
    #[serde(default)] path: String,
}

#[derive(Default, Deserialize)]
struct Audio {
    #[serde(default)] codec: String,
    sample_rate: Option<u32>,
    bits_per_sample: Option<u32>,
    channels: Option<u32>,
    bit_rate: Option<u64>,
}

#[derive(Deserialize)]
struct Tags {
    #[serde(rename = "DATE")]
    date: Option<Vec<String>>,
}

impl Info {
    pub(super) fn into_details(self, id: &str) -> Result<TrackDetails> {
        let Self { track, audio, tags } = self;
        ensure!(track.id == id, "jastreamer returned details for a different track");
        let Audio { codec, sample_rate, bits_per_sample, channels, bit_rate } = audio.unwrap_or_default();
        let date = tags.and_then(|tags| tags.date)
            .and_then(|dates| dates.into_iter().find(|date| !date.trim().is_empty())).unwrap_or_default();
        let mut genres = track.genres.unwrap_or_default();
        genres.retain(|genre| !genre.trim().is_empty());
        Ok(TrackDetails {
            album_artist: track.album_artist, date, genres,
            disc: track.disc.filter(|n| *n > 0), number: track.track.filter(|n| *n > 0),
            codec, sample_rate: sample_rate.filter(|n| *n > 0),
            bits_per_sample: bits_per_sample.filter(|n| *n > 0), channels: channels.filter(|n| *n > 0),
            bit_rate: bit_rate.filter(|n| *n > 0), play_count: track.play_count, path: track.path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn track_info_uses_decoded_values_and_requires_the_requested_id() {
        let wire = json!({
            "track": {"id": "a/b?", "artist": "Not the album artist", "format": "m4a", "disc": 0,
                "track": 3, "genres": null, "path": "album/song.m4a"},
            "audio": {"codec": "AAC", "sample_rate": 44100, "bits_per_sample": null, "channels": 2,
                "bit_rate": 160000},
            "tags": {"DATE": ["", "2024-09-01"]}
        });
        let wrong = serde_json::from_value::<Info>(wire.clone()).unwrap().into_details("other");
        assert!(wrong.is_err());
        let details = serde_json::from_value::<Info>(wire).unwrap().into_details("a/b?").unwrap();
        assert_eq!(details.codec, "AAC");
        assert_eq!(details.sample_rate, Some(44100));
        assert_eq!(details.bits_per_sample, None);
        assert_eq!(details.channels, Some(2));
        assert_eq!(details.bit_rate, Some(160000));
        assert_eq!(details.disc, None);
        assert_eq!(details.number, Some(3));
        assert_eq!(details.date, "2024-09-01");
        assert_eq!(details.path, "album/song.m4a");
        assert!(details.album_artist.is_empty());
        assert!(details.genres.is_empty());
        let endpoint = url(&Url::parse("https://localhost/").unwrap(), "a/b?").unwrap();
        assert_eq!(endpoint.path(), "/api/v1/library/tracks/a%2Fb%3F/info");
    }
}
