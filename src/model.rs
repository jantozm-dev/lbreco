use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

pub const APP_ID: &str = "io.github.lbreco.LBreco";
pub const APP_NAME: &str = "lbreco";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listenbrainz: ListenBrainzConfig,
    pub ai: AiConfig,
    pub discovery: DiscoveryConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listenbrainz: ListenBrainzConfig::default(),
            ai: AiConfig::default(),
            discovery: DiscoveryConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListenBrainzConfig {
    pub username: String,
    /// Optional. Needed for private profiles and higher rate limits.
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub enabled: bool,
    /// OpenAI-compatible API root, e.g. https://api.openai.com/v1
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://api.openai.com/v1".to_string(),
            model: String::new(),
            api_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscoveryConfig {
    pub count: usize,
    pub mode: DiscoveryMode,
    pub include_genres: bool,
    /// Skip tracks this app has recommended before.
    pub skip_recommended: bool,
    /// Only skip tracks recommended in the last N days. 0 means every pick.
    pub avoid_repeats_days: u32,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            count: 50,
            mode: DiscoveryMode::Mixed,
            include_genres: true,
            skip_recommended: true,
            avoid_repeats_days: 30,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryMode {
    Easy,
    Medium,
    Hard,
    Mixed,
}

impl DiscoveryMode {
    pub const ALL: [Self; 4] = [Self::Mixed, Self::Easy, Self::Medium, Self::Hard];

    pub fn label(self) -> &'static str {
        match self {
            Self::Easy => "Easy — familiar lane",
            Self::Medium => "Medium — a few steps out",
            Self::Hard => "Hard — deep exploration",
            Self::Mixed => "Mixed — best balance",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
            Self::Mixed => "mixed",
        }
    }

    pub fn from_key(value: &str) -> Self {
        match value {
            "easy" => Self::Easy,
            "medium" => Self::Medium,
            "hard" => Self::Hard,
            _ => Self::Mixed,
        }
    }
}

impl fmt::Display for DiscoveryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl Config {
    pub fn ai_ready(&self) -> bool {
        self.ai.enabled && !self.ai.base_url.trim().is_empty() && !self.ai.model.trim().is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.listenbrainz.username.trim().is_empty() {
            return Err("Add your ListenBrainz username in Preferences.".to_string());
        }
        if self.discovery.count == 0 {
            return Err("Pick at least one track.".to_string());
        }
        if self.ai.enabled && !self.ai_ready() {
            return Err("AI is enabled, but the API URL or model is empty.".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtistStat {
    pub name: String,
    pub mbid: Option<String>,
    pub listen_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordingStat {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub mbid: Option<String>,
    pub listen_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseStat {
    pub name: String,
    pub artist: String,
    pub listen_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecentTrack {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub mbid: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenreStat {
    pub name: String,
    pub listen_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EraStat {
    pub year: i32,
    pub listen_count: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TasteProfile {
    pub username: String,
    pub listen_count: u64,
    pub artists: Vec<ArtistStat>,
    pub recordings: Vec<RecordingStat>,
    pub releases: Vec<ReleaseStat>,
    pub recent: Vec<RecentTrack>,
    pub genres: Vec<GenreStat>,
    pub eras: Vec<EraStat>,
    pub known_mbids: HashSet<String>,
    pub known_keys: HashSet<String>,
}

impl TasteProfile {
    pub fn top_artist_names(&self) -> Vec<String> {
        self.artists
            .iter()
            .take(12)
            .map(|artist| artist.name.clone())
            .collect()
    }

    pub fn top_genre_names(&self) -> Vec<String> {
        self.genres
            .iter()
            .take(10)
            .map(|genre| genre.name.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    RadioEasy,
    RadioMedium,
    RadioHard,
    Genre,
    Collaborative,
}

impl CandidateSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::RadioEasy => "Familiar adjacent",
            Self::RadioMedium => "Similar artist",
            Self::RadioHard => "Deeper cut",
            Self::Genre => "Genre explorer",
            Self::Collaborative => "Listeners like you",
        }
    }

    pub fn weight(self) -> f64 {
        match self {
            // ListenBrainz's weekly playlists are built purely from raw
            // collaborative-filtering recommendations, so the CF source leads.
            Self::Collaborative => 1.04,
            Self::RadioHard => 0.98,
            Self::RadioMedium => 0.84,
            Self::Genre => 0.72,
            Self::RadioEasy => 0.64,
        }
    }

    pub fn is_exploratory(self) -> bool {
        matches!(self, Self::RadioHard | Self::Collaborative | Self::Genre)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub mbid: String,
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub duration_ms: Option<u64>,
    pub tags: Vec<String>,
    pub source: CandidateSource,
    pub seed_artist: Option<String>,
    pub similar_artist: Option<String>,
    #[serde(default)]
    pub popularity: u64,
    #[serde(default)]
    pub cf_score: f64,
    pub first_release_date: Option<String>,
}

impl Candidate {
    pub fn key(&self) -> String {
        normalize_key(&self.artist, &self.title)
    }

    pub fn musicbrainz_url(&self) -> String {
        format!("https://musicbrainz.org/recording/{}", self.mbid)
    }

    pub fn youtube_url(&self) -> String {
        format!(
            "https://www.youtube.com/results?search_query={}",
            urlencoding::encode(&format!("{} {}", self.artist, self.title))
        )
    }
}

impl Default for CandidateSource {
    fn default() -> Self {
        Self::RadioMedium
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    pub rank: usize,
    pub mbid: String,
    pub artist: String,
    pub title: String,
    pub album: Option<String>,
    pub year: Option<i32>,
    pub duration_ms: Option<u64>,
    pub reason: String,
    pub source: CandidateSource,
    pub seed_artist: Option<String>,
    pub tags: Vec<String>,
    pub musicbrainz_url: String,
    pub youtube_url: String,
}

impl From<&Candidate> for Recommendation {
    fn from(value: &Candidate) -> Self {
        Self {
            rank: 0,
            mbid: value.mbid.clone(),
            artist: value.artist.clone(),
            title: value.title.clone(),
            album: value.album.clone(),
            year: value.year,
            duration_ms: value.duration_ms,
            reason: String::new(),
            source: value.source,
            seed_artist: value.seed_artist.clone(),
            tags: value.tags.clone(),
            musicbrainz_url: value.musicbrainz_url(),
            youtube_url: value.youtube_url(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunSummary {
    pub username: String,
    pub generated_at: String,
    pub listen_count: u64,
    pub top_artists: Vec<String>,
    pub top_genres: Vec<String>,
    pub ai_used: bool,
    pub ai_model: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunResult {
    pub summary: RunSummary,
    pub recommendations: Vec<Recommendation>,
}

#[derive(Debug, Clone)]
pub enum Progress {
    Stage { fraction: f64, text: String },
    Done(Box<RunResult>),
    Failed(String),
    Cancelled,
}

pub fn normalize_key(artist: &str, title: &str) -> String {
    let clean = |value: &str| {
        value
            .to_lowercase()
            .chars()
            .filter(|ch| ch.is_alphanumeric())
            .collect::<String>()
    };
    format!("{}|{}", clean(artist), clean(title))
}
