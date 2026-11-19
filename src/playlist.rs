//! The accepted-tracks playlist and its export formats.
//!
//! lbreco does not download audio. Songs the listener keeps (checkboxes in the
//! list, or "Add to playlist" on the swipe card) are collected here and can be
//! exported as a CSV/JSON/M3U file for Spotify, YouTube Music, TuneMyMusic,
//! Soundiiz or any other playlist importer.

use std::fs;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config;
use crate::model::{Recommendation, normalize_key};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub mbid: String,
    pub artist: String,
    pub title: String,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub added_at: String,
}

impl PlaylistTrack {
    pub fn key(&self) -> String {
        let mbid = self.mbid.trim();
        if mbid.is_empty() {
            normalize_key(&self.artist, &self.title)
        } else {
            format!("mbid:{mbid}")
        }
    }

    pub fn musicbrainz_url(&self) -> String {
        let mbid = self.mbid.trim();
        if mbid.is_empty() {
            String::new()
        } else {
            format!("https://musicbrainz.org/recording/{mbid}")
        }
    }

    pub fn youtube_url(&self) -> String {
        format!(
            "https://www.youtube.com/results?search_query={}",
            urlencoding::encode(&search_text(self))
        )
    }

    pub fn youtube_music_url(&self) -> String {
        format!(
            "https://music.youtube.com/search?q={}",
            urlencoding::encode(&search_text(self))
        )
    }

    pub fn spotify_search_url(&self) -> String {
        format!(
            "https://open.spotify.com/search/{}",
            urlencoding::encode(&search_text(self))
        )
    }
}

impl From<&Recommendation> for PlaylistTrack {
    fn from(value: &Recommendation) -> Self {
        Self {
            mbid: value.mbid.clone(),
            artist: value.artist.clone(),
            title: value.title.clone(),
            album: value.album.clone(),
            year: value.year,
            duration_ms: value.duration_ms,
            reason: value.reason.clone(),
            added_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        }
    }
}

fn search_text(track: &PlaylistTrack) -> String {
    format!("{} {}", track.artist.trim(), track.title.trim())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Playlist {
    #[serde(default)]
    pub tracks: Vec<PlaylistTrack>,
}

impl Playlist {
    pub fn load() -> Self {
        let path = config::playlist_path();
        match fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|error| {
                eprintln!("lbreco: could not parse {}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config::playlist_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn contains(&self, mbid: &str) -> bool {
        let key = format!("mbid:{}", mbid.trim());
        self.tracks.iter().any(|track| track.key() == key)
    }

    /// Returns true when the track was actually new.
    pub fn add(&mut self, recommendation: &Recommendation) -> bool {
        if self.contains(&recommendation.mbid) {
            return false;
        }
        self.tracks.push(PlaylistTrack::from(recommendation));
        true
    }

    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.tracks.len();
        self.tracks.retain(|track| track.key() != key);
        self.tracks.len() != before
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
    }
}

/// CSV for TuneMyMusic, Soundiiz and similar playlist importers. These expect
/// the three columns `Track name`, `Artist name` and `Album name`.
pub fn to_csv(playlist: &Playlist) -> String {
    let mut out = String::from("Track name,Artist name,Album name\n");
    for track in &playlist.tracks {
        let fields = [
            track.title.clone(),
            track.artist.clone(),
            track.album.clone().unwrap_or_default(),
        ];
        let row = fields.map(|field| csv_field(&field)).join(",");
        out.push_str(&row);
        out.push('\n');
    }
    out
}

pub fn to_json(playlist: &Playlist) -> Result<String> {
    let tracks: Vec<serde_json::Value> = playlist
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let mut item = json!({
                "position": index + 1,
                "title": track.title,
                "artist": track.artist,
                "mbid": track.mbid,
                "musicbrainz_url": track.musicbrainz_url(),
                "spotify_search_url": track.spotify_search_url(),
                "youtube_music_url": track.youtube_music_url(),
                "youtube_url": track.youtube_url(),
                "added_at": track.added_at,
            });
            if let Some(album) = track
                .album
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                item["album"] = json!(album);
            }
            if let Some(year) = track.year {
                item["year"] = json!(year);
            }
            if let Some(duration) = track.duration_ms {
                item["duration_ms"] = json!(duration);
            }
            if !track.reason.trim().is_empty() {
                item["reason"] = json!(track.reason);
            }
            item
        })
        .collect();

    let document = json!({
        "name": "lbreco playlist",
        "generator": "lbreco",
        "created_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "count": tracks.len(),
        "tracks": tracks,
    });
    Ok(serde_json::to_string_pretty(&document)?)
}

/// The M3U entries are YouTube Music searches, so the file opens in VLC or a
/// browser and still points at playable sources.
pub fn to_m3u(playlist: &Playlist) -> String {
    let mut out = String::from("#EXTM3U\n#PLAYLIST:lbreco playlist\n");
    for track in &playlist.tracks {
        out.push_str(&format!(
            "#EXTINF:-1,{} - {}\n{}\n",
            track.artist,
            track.title,
            track.youtube_music_url()
        ));
    }
    out
}

/// One `Artist - Title` line per track, ready to paste into a search box or a
/// playlist importer that accepts free text.
pub fn to_plain_text(playlist: &Playlist) -> String {
    playlist
        .tracks
        .iter()
        .map(|track| format!("{} - {}", track.artist, track.title))
        .collect::<Vec<_>>()
        .join("\n")
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub fn suggested_name(extension: &str) -> String {
    let date = Utc::now().format("%Y-%m-%d");
    format!("lbreco-playlist-{date}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CandidateSource, Recommendation};

    fn recommendation(title: &str, artist: &str, album: Option<&str>) -> Recommendation {
        let mbid = if title.contains("Two") {
            "22222222-2222-2222-2222-222222222222"
        } else {
            "11111111-1111-1111-1111-111111111111"
        };
        Recommendation {
            rank: 1,
            mbid: mbid.to_string(),
            artist: artist.to_string(),
            title: title.to_string(),
            album: album.map(str::to_string),
            year: Some(2020),
            duration_ms: Some(200_000),
            reason: "Because you like something.".to_string(),
            source: CandidateSource::Collaborative,
            seed_artist: None,
            tags: Vec::new(),
            musicbrainz_url: format!("https://musicbrainz.org/recording/{mbid}"),
            youtube_url: "https://www.youtube.com/results?search_query=x".to_string(),
        }
    }

    fn playlist() -> Playlist {
        let mut playlist = Playlist::default();
        playlist.add(&recommendation(
            "Song One",
            "The Comma, Band",
            Some("First Album"),
        ));
        playlist.add(&recommendation("Song \"Two\"", "Beta", None));
        playlist
    }

    #[test]
    fn add_ignores_duplicate_mbids() {
        let mut playlist = Playlist::default();
        assert!(playlist.add(&recommendation("Song One", "Artist", Some("Album"))));
        assert!(!playlist.add(&recommendation("Song One", "Artist", Some("Album"))));
        assert_eq!(playlist.len(), 1);
    }

    #[test]
    fn remove_and_clear_work_by_key() {
        let mut playlist = playlist();
        let key = playlist.tracks[0].key();
        assert!(playlist.remove(&key));
        assert!(!playlist.remove(&key));
        assert_eq!(playlist.len(), 1);
        playlist.clear();
        assert!(playlist.is_empty());
    }

    #[test]
    fn csv_uses_importer_headers_and_escapes_fields() {
        let text = to_csv(&playlist());
        assert_eq!(
            text,
            concat!(
                "Track name,Artist name,Album name\n",
                "Song One,\"The Comma, Band\",First Album\n",
                "\"Song \"\"Two\"\"\",Beta,\n",
            )
        );
    }

    #[test]
    fn json_contains_search_links_and_positions() {
        let value: serde_json::Value =
            serde_json::from_str(&to_json(&playlist()).unwrap()).unwrap();
        assert_eq!(value["count"], 2);
        let tracks = value["tracks"].as_array().unwrap();
        assert_eq!(tracks[0]["position"], 1);
        assert_eq!(tracks[1]["position"], 2);
        assert!(
            tracks[0]["spotify_search_url"]
                .as_str()
                .unwrap()
                .starts_with("https://open.spotify.com/search/")
        );
        assert!(
            tracks[0]["youtube_music_url"]
                .as_str()
                .unwrap()
                .starts_with("https://music.youtube.com/search")
        );
        assert_eq!(tracks[1]["artist"], "Beta");
        assert!(tracks[1].get("album").is_none());
    }

    #[test]
    fn plain_text_is_one_line_per_track() {
        assert_eq!(
            to_plain_text(&playlist()),
            "The Comma, Band - Song One\nBeta - Song \"Two\""
        );
    }

    #[test]
    fn m3u_has_extinf_and_search_urls() {
        let text = to_m3u(&playlist());
        assert!(text.starts_with("#EXTM3U\n"));
        assert!(text.contains("#EXTINF:-1,The Comma, Band - Song One\n"));
        assert!(text.contains("https://music.youtube.com/search?q="));
    }
}
