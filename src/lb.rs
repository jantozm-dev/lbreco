use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use reqwest::{Client, Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use crate::model::{
    ArtistStat, EraStat, GenreStat, RecentTrack, RecordingStat, ReleaseStat, TasteProfile, VERSION,
    normalize_key,
};

const API_ROOT: &str = "https://api.listenbrainz.org/1";
const MIN_INTERVAL: Duration = Duration::from_millis(1050);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioMode {
    Easy,
    Medium,
    Hard,
}

impl RadioMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
        }
    }

    /// Popularity window (0 = least popular, 100 = most popular on ListenBrainz).
    pub fn pop_range(self) -> (u8, u8) {
        match self {
            Self::Easy => (35, 100),
            Self::Medium => (10, 85),
            Self::Hard => (0, 55),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RadioTrack {
    pub recording_mbid: String,
    pub similar_artist_mbid: Option<String>,
    pub similar_artist_name: Option<String>,
    pub total_listen_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct RadioTagTrack {
    pub recording_mbid: String,
    /// The API returns a percentage as a float, e.g. 26.261587097465572.
    pub percent: f64,
    pub tag_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CfRecommendation {
    pub recording_mbid: String,
    pub score: f64,
    pub latest_listened_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    pub mbid: String,
    pub title: String,
    pub artist: String,
    pub artist_mbid: Option<String>,
    pub album: Option<String>,
    pub release_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    pub year: Option<i32>,
    pub duration_ms: Option<u64>,
    pub tags: Vec<String>,
    pub first_release_date: Option<String>,
}

/// Recordings and related entities the listener never wants recommended again.
#[derive(Debug, Clone, Default)]
pub struct Exclusions {
    pub recordings: HashSet<String>,
    pub artists: HashSet<String>,
    pub releases: HashSet<String>,
    pub release_groups: HashSet<String>,
}

impl Exclusions {
    pub fn blocks_recording(&self, mbid: &str) -> bool {
        !mbid.is_empty() && self.recordings.contains(mbid)
    }

    pub fn blocks_metadata(&self, metadata: &TrackMetadata) -> bool {
        metadata
            .artist_mbid
            .as_deref()
            .is_some_and(|mbid| self.artists.contains(mbid))
            || metadata
                .release_mbid
                .as_deref()
                .is_some_and(|mbid| self.releases.contains(mbid))
            || metadata
                .release_group_mbid
                .as_deref()
                .is_some_and(|mbid| self.release_groups.contains(mbid))
    }
}

pub struct LbClient {
    http: Client,
    username: String,
    token: String,
    gate: Mutex<Instant>,
}

impl LbClient {
    pub fn new(username: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let username = username.into().trim().to_string();
        if username.is_empty() {
            bail!("Add your ListenBrainz username in Preferences.");
        }
        let token = token.into().trim().to_string();
        let user_agent = format!("lbreco/{VERSION} ( https://listenbrainz.org/user/{username} )");
        let http = Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(45))
            .build()
            .context("creating the HTTP client")?;
        Ok(Self {
            http,
            username,
            token,
            gate: Mutex::new(Instant::now() - MIN_INTERVAL),
        })
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    async fn throttle(&self) {
        let mut last = self.gate.lock().await;
        let elapsed = last.elapsed();
        if elapsed < MIN_INTERVAL {
            tokio::time::sleep(MIN_INTERVAL - elapsed).await;
        }
        *last = Instant::now();
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&serde_json::Value>,
    ) -> Result<Option<T>> {
        let url = format!("{API_ROOT}/{path}");
        let mut last_error = String::new();

        for attempt in 0..3 {
            self.throttle().await;
            let mut request = self.http.request(method.clone(), &url).query(query);
            if !self.token.is_empty() {
                request = request.header("Authorization", format!("Token {}", self.token));
            }
            if let Some(body) = body {
                request = request.json(body);
            }

            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("network error: {error}");
                    if attempt < 2 {
                        tokio::time::sleep(Duration::from_secs(2 + attempt as u64 * 3)).await;
                        continue;
                    }
                    bail!("Could not reach ListenBrainz: {error}");
                }
            };

            let status = response.status();
            if status == StatusCode::NO_CONTENT {
                return Ok(None);
            }

            if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                let reset_in = response
                    .headers()
                    .get("x-ratelimit-reset-in")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(2 + attempt as u64 * 3);
                last_error = format!("ListenBrainz returned HTTP {status}");
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_secs(reset_in.clamp(1, 30))).await;
                    continue;
                }
                bail!("{last_error}. Try again in a moment.");
            }

            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                bail!(
                    "ListenBrainz refused access to this profile. If it is private, add a user token in Preferences."
                );
            }

            if status == StatusCode::NOT_FOUND {
                bail!("ListenBrainz user '{}' was not found.", self.username);
            }

            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let snippet = body.chars().take(240).collect::<String>();
                bail!(
                    "ListenBrainz request failed with HTTP {status}: {}",
                    snippet.trim()
                );
            }

            let bytes = response
                .bytes()
                .await
                .context("reading ListenBrainz response")?;
            if bytes.is_empty() {
                return Ok(None);
            }
            return serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing ListenBrainz response from {path}"))
                .map(Some);
        }

        bail!("{last_error}")
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<T>> {
        self.request(Method::GET, path, query, None).await
    }

    pub async fn listen_count(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            count: u64,
        }
        let response: Envelope = self
            .get(&format!("user/{}/listen-count", self.username), &[])
            .await?
            .context("ListenBrainz returned no listen count")?;
        Ok(response.payload.count)
    }

    pub async fn top_artists(&self, range: &str, count: usize) -> Result<Vec<ArtistStat>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            artists: Vec<ApiArtist>,
        }
        #[derive(Deserialize)]
        struct ApiArtist {
            artist_name: String,
            artist_mbid: Option<String>,
            #[serde(default)]
            listen_count: u64,
        }

        let count = count.min(1000).to_string();
        let response: Option<Envelope> = self
            .get(
                &format!("stats/user/{}/artists", self.username),
                &[("range", range.to_string()), ("count", count)],
            )
            .await?;
        Ok(response
            .map(|envelope| {
                envelope
                    .payload
                    .artists
                    .into_iter()
                    .map(|artist| ArtistStat {
                        name: artist.artist_name,
                        mbid: artist.artist_mbid.filter(|value| !value.is_empty()),
                        listen_count: artist.listen_count,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn top_recordings(&self, range: &str, count: usize) -> Result<Vec<RecordingStat>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            recordings: Vec<ApiRecording>,
        }
        #[derive(Deserialize)]
        struct ApiRecording {
            track_name: String,
            artist_name: String,
            release_name: Option<String>,
            recording_mbid: Option<String>,
            #[serde(default)]
            listen_count: u64,
        }

        let count = count.min(1000).to_string();
        let response: Option<Envelope> = self
            .get(
                &format!("stats/user/{}/recordings", self.username),
                &[("range", range.to_string()), ("count", count)],
            )
            .await?;
        Ok(response
            .map(|envelope| {
                envelope
                    .payload
                    .recordings
                    .into_iter()
                    .map(|track| RecordingStat {
                        title: track.track_name,
                        artist: track.artist_name,
                        album: track.release_name.filter(|value| !value.is_empty()),
                        mbid: track.recording_mbid.filter(|value| !value.is_empty()),
                        listen_count: track.listen_count,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn top_releases(&self, range: &str, count: usize) -> Result<Vec<ReleaseStat>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            releases: Vec<ApiRelease>,
        }
        #[derive(Deserialize)]
        struct ApiRelease {
            release_name: String,
            artist_name: String,
            #[serde(default)]
            listen_count: u64,
        }

        let count = count.min(1000).to_string();
        let response: Option<Envelope> = self
            .get(
                &format!("stats/user/{}/releases", self.username),
                &[("range", range.to_string()), ("count", count)],
            )
            .await?;
        Ok(response
            .map(|envelope| {
                envelope
                    .payload
                    .releases
                    .into_iter()
                    .map(|release| ReleaseStat {
                        name: release.release_name,
                        artist: release.artist_name,
                        listen_count: release.listen_count,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn recent_listens(&self, count: usize) -> Result<Vec<RecentTrack>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            listens: Vec<ApiListen>,
        }
        #[derive(Deserialize)]
        struct ApiListen {
            track_metadata: ApiTrackMetadata,
        }
        #[derive(Deserialize)]
        struct ApiTrackMetadata {
            artist_name: String,
            track_name: String,
            release_name: Option<String>,
            mbid_mapping: Option<ApiMbidMapping>,
            additional_info: Option<ApiAdditionalInfo>,
        }
        #[derive(Deserialize)]
        struct ApiMbidMapping {
            recording_mbid: Option<String>,
        }
        #[derive(Deserialize)]
        struct ApiAdditionalInfo {
            recording_mbid: Option<String>,
        }

        let count = count.min(1000).to_string();
        let response: Option<Envelope> = self
            .get(
                &format!("user/{}/listens", self.username),
                &[("count", count)],
            )
            .await?;
        Ok(response
            .map(|envelope| {
                envelope
                    .payload
                    .listens
                    .into_iter()
                    .map(|listen| {
                        let metadata = listen.track_metadata;
                        let mbid = metadata
                            .mbid_mapping
                            .and_then(|mapping| mapping.recording_mbid)
                            .or_else(|| {
                                metadata
                                    .additional_info
                                    .and_then(|info| info.recording_mbid)
                            })
                            .filter(|value| !value.is_empty());
                        RecentTrack {
                            title: metadata.track_name,
                            artist: metadata.artist_name,
                            album: metadata.release_name.filter(|value| !value.is_empty()),
                            mbid,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn genre_activity(&self) -> Result<Vec<GenreStat>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            genre_activity: Vec<ApiGenre>,
        }
        #[derive(Deserialize)]
        struct ApiGenre {
            genre: String,
            #[serde(default)]
            listen_count: u64,
        }

        let response: Option<Envelope> = self
            .get(&format!("stats/user/{}/genre-activity", self.username), &[])
            .await?;
        let mut totals: HashMap<String, u64> = HashMap::new();
        if let Some(envelope) = response {
            for item in envelope.payload.genre_activity {
                *totals.entry(item.genre).or_default() += item.listen_count;
            }
        }
        let mut genres: Vec<GenreStat> = totals
            .into_iter()
            .map(|(name, listen_count)| GenreStat { name, listen_count })
            .collect();
        genres.sort_by_key(|genre| std::cmp::Reverse(genre.listen_count));
        genres.truncate(10);
        Ok(genres)
    }

    pub async fn era_activity(&self) -> Result<Vec<EraStat>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            era_activity: Vec<ApiEra>,
        }
        #[derive(Deserialize)]
        struct ApiEra {
            year: i32,
            #[serde(default)]
            listen_count: u64,
        }

        let response: Option<Envelope> = self
            .get(
                &format!("stats/user/{}/era-activity", self.username),
                &[("range", "all_time".to_string())],
            )
            .await?;
        let mut eras: Vec<EraStat> = response
            .map(|envelope| {
                envelope
                    .payload
                    .era_activity
                    .into_iter()
                    .map(|era| EraStat {
                        year: era.year,
                        listen_count: era.listen_count,
                    })
                    .collect()
            })
            .unwrap_or_default();
        eras.sort_by_key(|era| era.year);
        Ok(eras)
    }

    pub async fn taste_profile(&self) -> Result<TasteProfile> {
        let listen_count = self.listen_count().await?;
        if listen_count == 0 {
            bail!(
                "This ListenBrainz account has no listens yet. Scrobble something first, then try again."
            );
        }

        let artists = self.top_artists("all_time", 100).await?;
        let recordings = self.top_recordings("all_time", 100).await?;
        let releases = self.top_releases("all_time", 50).await?;
        let recent = self.recent_listens(100).await?;
        let genres = self.genre_activity().await?;
        let eras = self.era_activity().await?;

        let mut known_mbids: HashSet<String> = HashSet::new();
        let mut known_keys: HashSet<String> = HashSet::new();
        for recording in &recordings {
            if let Some(mbid) = &recording.mbid {
                known_mbids.insert(mbid.clone());
            }
            known_keys.insert(normalize_key(&recording.artist, &recording.title));
        }
        for listen in &recent {
            if let Some(mbid) = &listen.mbid {
                known_mbids.insert(mbid.clone());
            }
            known_keys.insert(normalize_key(&listen.artist, &listen.title));
        }

        Ok(TasteProfile {
            username: self.username.clone(),
            listen_count,
            artists,
            recordings,
            releases,
            recent,
            genres,
            eras,
            known_mbids,
            known_keys,
        })
    }

    /// Raw collaborative-filtering recommendations, paginated the same way
    /// ListenBrainz's own weekly playlists fetch them. Raw scores are tiny
    /// (around 0.06) and are normalised by the caller.
    pub async fn cf_recommendations(&self, limit: usize) -> Result<Vec<CfRecommendation>> {
        #[derive(Deserialize)]
        struct Envelope {
            payload: Payload,
        }
        #[derive(Deserialize)]
        struct Payload {
            #[serde(default)]
            mbids: Vec<ApiRecommendation>,
            #[serde(default)]
            total_mbid_count: usize,
        }
        #[derive(Deserialize)]
        struct ApiRecommendation {
            recording_mbid: String,
            #[serde(default)]
            score: f64,
            latest_listened_at: Option<String>,
        }

        const PAGE: usize = 100;
        let limit = limit.min(1000);
        let mut output = Vec::new();
        let mut offset = 0;
        let mut seen = HashSet::new();

        while output.len() < limit && offset < limit {
            let count = PAGE.min(limit - offset);
            let response: Option<Envelope> = self
                .get(
                    &format!("cf/recommendation/user/{}/recording", self.username),
                    &[
                        ("artist_type", "raw".to_string()),
                        ("count", count.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let Some(envelope) = response else {
                break;
            };
            let total = envelope.payload.total_mbid_count;
            let received = envelope.payload.mbids.len();
            for item in envelope.payload.mbids {
                if item.recording_mbid.trim().is_empty()
                    || !seen.insert(item.recording_mbid.clone())
                {
                    continue;
                }
                output.push(CfRecommendation {
                    recording_mbid: item.recording_mbid,
                    score: item.score,
                    latest_listened_at: item.latest_listened_at,
                });
                if output.len() >= limit {
                    break;
                }
            }
            if received < count || offset + received >= total {
                break;
            }
            offset += received;
        }

        Ok(output)
    }

    /// Hated/disliked recommendations plus explicit do-not-recommend entries.
    /// Both endpoints work without a token for public profiles; a token simply
    /// makes private profiles work too.
    pub async fn exclusions(&self) -> Result<Exclusions> {
        let mut exclusions = Exclusions::default();

        #[derive(Deserialize)]
        struct FeedbackEnvelope {
            #[serde(default)]
            feedback: Vec<ApiFeedback>,
            #[serde(default)]
            total_count: usize,
        }
        #[derive(Deserialize)]
        struct ApiFeedback {
            recording_mbid: Option<String>,
            #[serde(default)]
            score: i32,
        }

        const PAGE: usize = 100;
        let mut offset = 0;
        loop {
            let response: Option<FeedbackEnvelope> = self
                .get(
                    &format!("feedback/user/{}/get-feedback", self.username),
                    &[
                        ("score", "-1".to_string()),
                        ("count", PAGE.to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            let Some(envelope) = response else {
                break;
            };
            let received = envelope.feedback.len();
            for item in envelope.feedback {
                if item.score < 0 {
                    if let Some(mbid) = item.recording_mbid.filter(|v| !v.is_empty()) {
                        exclusions.recordings.insert(mbid);
                    }
                }
            }
            offset += received;
            if received < PAGE || offset >= envelope.total_count {
                break;
            }
        }

        #[derive(Deserialize)]
        struct DnrEnvelope {
            #[serde(default)]
            results: Vec<ApiDnr>,
            #[serde(default)]
            total_count: usize,
        }
        #[derive(Deserialize)]
        struct ApiDnr {
            entity: String,
            entity_mbid: String,
        }

        let mut offset = 0;
        loop {
            let response: Option<DnrEnvelope> = self
                .get(
                    &format!("user/{}/do-not-recommend", self.username),
                    &[("count", PAGE.to_string()), ("offset", offset.to_string())],
                )
                .await?;
            let Some(envelope) = response else {
                break;
            };
            let received = envelope.results.len();
            for item in envelope.results {
                let mbid = item.entity_mbid.trim().to_string();
                if mbid.is_empty() {
                    continue;
                }
                match item.entity.as_str() {
                    "recording" => {
                        exclusions.recordings.insert(mbid);
                    }
                    "artist" => {
                        exclusions.artists.insert(mbid);
                    }
                    "release" => {
                        exclusions.releases.insert(mbid);
                    }
                    "release_group" => {
                        exclusions.release_groups.insert(mbid);
                    }
                    _ => {}
                }
            }
            offset += received;
            if received < PAGE || offset >= envelope.total_count {
                break;
            }
        }

        Ok(exclusions)
    }

    pub async fn radio_artist(
        &self,
        artist_mbid: &str,
        mode: RadioMode,
        max_similar_artists: u8,
        max_recordings_per_artist: u8,
    ) -> Result<Vec<RadioTrack>> {
        #[derive(Deserialize)]
        struct ApiRadioTrack {
            recording_mbid: String,
            similar_artist_mbid: Option<String>,
            similar_artist_name: Option<String>,
            #[serde(default)]
            total_listen_count: u64,
        }

        let (pop_begin, pop_end) = mode.pop_range();
        let response: Option<HashMap<String, Vec<ApiRadioTrack>>> = self
            .get(
                &format!("lb-radio/artist/{artist_mbid}"),
                &[
                    ("mode", mode.as_str().to_string()),
                    ("max_similar_artists", max_similar_artists.to_string()),
                    (
                        "max_recordings_per_artist",
                        max_recordings_per_artist.to_string(),
                    ),
                    ("pop_begin", pop_begin.to_string()),
                    ("pop_end", pop_end.to_string()),
                ],
            )
            .await?;

        let mut tracks = Vec::new();
        let mut seen = HashSet::new();
        for (_, values) in response.unwrap_or_default() {
            for item in values {
                if item.recording_mbid.is_empty() || !seen.insert(item.recording_mbid.clone()) {
                    continue;
                }
                tracks.push(RadioTrack {
                    recording_mbid: item.recording_mbid,
                    similar_artist_mbid: item.similar_artist_mbid,
                    similar_artist_name: item.similar_artist_name,
                    total_listen_count: item.total_listen_count,
                });
            }
        }
        Ok(tracks)
    }

    pub async fn radio_tags(&self, tag: &str, count: usize) -> Result<Vec<RadioTagTrack>> {
        #[derive(Deserialize)]
        struct ApiTagTrack {
            recording_mbid: String,
            #[serde(default)]
            percent: f64,
            #[serde(default)]
            tag_count: u64,
        }

        let response: Option<Vec<ApiTagTrack>> = self
            .get(
                "lb-radio/tags",
                &[
                    ("tag", tag.to_string()),
                    ("pop_begin", "5".to_string()),
                    ("pop_end", "95".to_string()),
                    ("count", count.min(100).to_string()),
                ],
            )
            .await?;
        let mut tracks = Vec::new();
        let mut seen = HashSet::new();
        for item in response.unwrap_or_default() {
            if !item.recording_mbid.is_empty() && seen.insert(item.recording_mbid.clone()) {
                tracks.push(RadioTagTrack {
                    recording_mbid: item.recording_mbid,
                    percent: item.percent,
                    tag_count: item.tag_count,
                });
            }
        }
        Ok(tracks)
    }

    pub async fn bulk_metadata(&self, mbids: &[String]) -> Result<HashMap<String, TrackMetadata>> {
        #[derive(Deserialize)]
        struct ApiMetadata {
            recording: Option<ApiRecording>,
            artist: Option<ApiArtist>,
            release: Option<ApiRelease>,
            tag: Option<ApiTags>,
        }
        #[derive(Deserialize)]
        struct ApiRecording {
            name: Option<String>,
            length: Option<u64>,
            first_release_date: Option<String>,
        }
        #[derive(Deserialize)]
        struct ApiArtist {
            name: Option<String>,
            #[serde(default)]
            artists: Vec<ApiArtistCredit>,
        }
        #[derive(Deserialize)]
        struct ApiArtistCredit {
            artist_mbid: Option<String>,
        }
        #[derive(Deserialize)]
        struct ApiRelease {
            name: Option<String>,
            mbid: Option<String>,
            release_group_mbid: Option<String>,
            year: Option<i32>,
        }
        #[derive(Deserialize)]
        struct ApiTags {
            #[serde(default)]
            recording: Vec<ApiTag>,
        }
        #[derive(Deserialize)]
        struct ApiTag {
            tag: String,
            #[serde(default)]
            count: u64,
        }

        let mut output = HashMap::new();
        for chunk in mbids.chunks(50) {
            let body = serde_json::json!({
                "recording_mbids": chunk,
                "inc": "artist release tag"
            });
            let response: Option<HashMap<String, ApiMetadata>> = self
                .request(Method::POST, "metadata/recording/", &[], Some(&body))
                .await?;

            for (mbid, item) in response.unwrap_or_default() {
                let Some(recording) = item.recording else {
                    continue;
                };
                let Some(title) = recording.name.filter(|value| !value.is_empty()) else {
                    continue;
                };
                let artist_mbid = item
                    .artist
                    .as_ref()
                    .and_then(|artist| artist.artists.first())
                    .and_then(|credit| credit.artist_mbid.clone())
                    .filter(|value| !value.is_empty());
                let Some(artist) = item
                    .artist
                    .and_then(|artist| artist.name)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let mut tags: Vec<ApiTag> = item
                    .tag
                    .map(|tags| tags.recording)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|tag| !tag.tag.trim().is_empty())
                    .collect();
                tags.sort_by_key(|tag| std::cmp::Reverse(tag.count));
                let album = item
                    .release
                    .as_ref()
                    .and_then(|release| release.name.clone())
                    .filter(|value| !value.is_empty());
                let release_mbid = item
                    .release
                    .as_ref()
                    .and_then(|release| release.mbid.clone())
                    .filter(|value| !value.is_empty());
                let release_group_mbid = item
                    .release
                    .as_ref()
                    .and_then(|release| release.release_group_mbid.clone())
                    .filter(|value| !value.is_empty());
                output.insert(
                    mbid.clone(),
                    TrackMetadata {
                        mbid,
                        title,
                        artist,
                        artist_mbid,
                        album,
                        release_mbid,
                        release_group_mbid,
                        year: item.release.and_then(|release| release.year),
                        duration_ms: recording.length,
                        tags: tags
                            .into_iter()
                            .take(3)
                            .map(|tag| tag.tag.to_lowercase())
                            .collect(),
                        first_release_date: recording.first_release_date,
                    },
                );
            }
        }
        Ok(output)
    }
}
