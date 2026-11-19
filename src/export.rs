use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;

use crate::model::{CandidateSource, Recommendation, RunResult};

pub fn to_json(result: &RunResult) -> Result<String> {
    Ok(serde_json::to_string_pretty(result)?)
}

pub fn to_csv(result: &RunResult) -> String {
    let mut out = String::from(
        "rank,artist,title,album,year,source,reason,mbid,musicbrainz_url,youtube_url\n",
    );

    for recommendation in &result.recommendations {
        let fields = [
            recommendation.rank.to_string(),
            recommendation.artist.clone(),
            recommendation.title.clone(),
            recommendation.album.clone().unwrap_or_default(),
            recommendation
                .year
                .map(|year| year.to_string())
                .unwrap_or_default(),
            source_key(recommendation.source).to_string(),
            recommendation.reason.clone(),
            recommendation.mbid.clone(),
            recommendation.musicbrainz_url.clone(),
            recommendation.youtube_url.clone(),
        ];
        let row = fields.map(|field| csv_field(&field)).join(",");
        out.push_str(&row);
        out.push('\n');
    }

    out
}

pub fn to_plain_text(result: &RunResult) -> String {
    let generated = generated_at(result);
    let date = date_only(&generated);
    let ai = match (result.summary.ai_used, result.summary.ai_model.as_deref()) {
        (true, Some(model)) if !model.trim().is_empty() => model.trim(),
        _ => "off",
    };

    let mut out = format!(
        "lbreco — {} picks for {}\nGenerated {} · AI: {}\n",
        result.recommendations.len(),
        result.summary.username,
        date,
        ai
    );

    for (index, recommendation) in result.recommendations.iter().enumerate() {
        out.push('\n');

        let rank = if recommendation.rank > 0 {
            recommendation.rank
        } else {
            index + 1
        };
        out.push_str(&format!(
            "{}. {} — {}",
            rank, recommendation.artist, recommendation.title
        ));

        let mut details = Vec::new();
        if let Some(album) = recommendation
            .album
            .as_deref()
            .map(str::trim)
            .filter(|album| !album.is_empty())
        {
            details.push(album.to_string());
        }
        if let Some(year) = recommendation.year {
            details.push(year.to_string());
        }
        if !details.is_empty() {
            out.push_str(&format!(" ({})", details.join(", ")));
        }
        out.push('\n');

        let reason = recommendation.reason.trim();
        if !reason.is_empty() {
            out.push_str("   ");
            out.push_str(reason);
            out.push('\n');
        }

        let url = recording_url(recommendation);
        if !url.is_empty() {
            out.push_str("   ");
            out.push_str(&url);
            out.push('\n');
        }
    }

    out
}

pub fn to_m3u(result: &RunResult) -> String {
    let mut out = String::from("#EXTM3U\n");
    out.push_str(&format!(
        "#PLAYLIST:lbreco for {}\n",
        result.summary.username
    ));

    for recommendation in &result.recommendations {
        out.push_str(&format!(
            "#EXTINF:-1,{} - {}\n",
            recommendation.artist, recommendation.title
        ));
        out.push_str(&recording_url(recommendation));
        out.push('\n');
    }

    out
}

pub fn to_jspf(result: &RunResult) -> Result<String> {
    let date = rfc3339(&generated_at(result));
    let title = format!(
        "lbreco for {} — {}",
        result.summary.username,
        date_only(&date)
    );

    let mut tracks = Vec::with_capacity(result.recommendations.len());
    for recommendation in &result.recommendations {
        let mut track = json!({
            "identifier": recording_url(recommendation),
            "title": recommendation.title.clone(),
            "creator": recommendation.artist.clone(),
        });

        if let Some(album) = recommendation
            .album
            .as_deref()
            .map(str::trim)
            .filter(|album| !album.is_empty())
        {
            track["album"] = json!(album);
        }
        if let Some(duration) = recommendation.duration_ms {
            track["duration"] = json!(duration);
        }

        tracks.push(track);
    }

    let playlist = json!({
        "playlist": {
            "title": title,
            "creator": "lbreco",
            "date": date,
            "track": tracks,
        }
    });

    Ok(serde_json::to_string_pretty(&playlist)?)
}

fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn source_key(source: CandidateSource) -> &'static str {
    match source {
        CandidateSource::RadioEasy => "radio_easy",
        CandidateSource::RadioMedium => "radio_medium",
        CandidateSource::RadioHard => "radio_hard",
        CandidateSource::Genre => "genre",
        CandidateSource::Collaborative => "collaborative",
    }
}

fn generated_at(result: &RunResult) -> String {
    let value = result.summary.generated_at.trim();
    if value.is_empty() {
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    } else {
        value.to_string()
    }
}

fn date_only(value: &str) -> &str {
    value
        .split(|ch| ch == 'T' || ch == ' ')
        .next()
        .unwrap_or(value)
}

fn rfc3339(value: &str) -> String {
    match DateTime::parse_from_rfc3339(value) {
        Ok(date) => date.to_rfc3339_opts(SecondsFormat::Secs, true),
        Err(_) => Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
    }
}

fn recording_url(recommendation: &Recommendation) -> String {
    let url = recommendation.musicbrainz_url.trim();
    if !url.is_empty() {
        return url.to_string();
    }

    let mbid = recommendation.mbid.trim();
    if mbid.is_empty() {
        String::new()
    } else {
        format!("https://musicbrainz.org/recording/{mbid}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CandidateSource, Recommendation, RunResult, RunSummary};

    const MBID_ONE: &str = "11111111-1111-1111-1111-111111111111";
    const MBID_TWO: &str = "22222222-2222-2222-2222-222222222222";
    const URL_ONE: &str = "https://musicbrainz.org/recording/11111111-1111-1111-1111-111111111111";
    const URL_TWO: &str = "https://musicbrainz.org/recording/22222222-2222-2222-2222-222222222222";

    fn sample() -> RunResult {
        RunResult {
            summary: RunSummary {
                username: "alice".to_string(),
                generated_at: "2026-11-17T12:34:56Z".to_string(),
                listen_count: 4321,
                top_artists: vec!["Seed Artist".to_string()],
                top_genres: vec!["indie".to_string()],
                ai_used: true,
                ai_model: Some("gpt-test".to_string()),
                warnings: Vec::new(),
            },
            recommendations: vec![
                Recommendation {
                    rank: 1,
                    mbid: MBID_ONE.to_string(),
                    artist: "The \"Comma\", Band".to_string(),
                    title: "Song One".to_string(),
                    album: Some("First, Album".to_string()),
                    year: Some(2020),
                    duration_ms: Some(215_000),
                    reason: "Because you like Seed Artist".to_string(),
                    source: CandidateSource::RadioMedium,
                    seed_artist: Some("Seed Artist".to_string()),
                    tags: vec!["indie".to_string()],
                    musicbrainz_url: URL_ONE.to_string(),
                    youtube_url:
                        "https://www.youtube.com/results?search_query=the-comma-band+song-one"
                            .to_string(),
                },
                Recommendation {
                    rank: 2,
                    mbid: MBID_TWO.to_string(),
                    artist: "Beta Artist".to_string(),
                    title: "Song \"Two\"".to_string(),
                    album: None,
                    year: None,
                    duration_ms: None,
                    reason: "A deeper cut".to_string(),
                    source: CandidateSource::Genre,
                    seed_artist: None,
                    tags: Vec::new(),
                    musicbrainz_url: URL_TWO.to_string(),
                    youtube_url:
                        "https://www.youtube.com/results?search_query=beta-artist+song-two"
                            .to_string(),
                },
            ],
        }
    }

    #[test]
    fn csv_escapes_quotes_and_commas() {
        let expected = concat!(
            "rank,artist,title,album,year,source,reason,mbid,musicbrainz_url,youtube_url\n",
            "1,\"The \"\"Comma\"\", Band\",Song One,\"First, Album\",2020,radio_medium,",
            "Because you like Seed Artist,11111111-1111-1111-1111-111111111111,",
            "https://musicbrainz.org/recording/11111111-1111-1111-1111-111111111111,",
            "https://www.youtube.com/results?search_query=the-comma-band+song-one\n",
            "2,Beta Artist,\"Song \"\"Two\"\"\",,,genre,A deeper cut,",
            "22222222-2222-2222-2222-222222222222,",
            "https://musicbrainz.org/recording/22222222-2222-2222-2222-222222222222,",
            "https://www.youtube.com/results?search_query=beta-artist+song-two\n",
        );

        assert_eq!(to_csv(&sample()), expected);
        assert!(to_csv(&sample()).contains("\"The \"\"Comma\"\", Band\""));
    }

    #[test]
    fn plain_text_lists_tracks_with_reasons() {
        let expected = concat!(
            "lbreco — 2 picks for alice\n",
            "Generated 2026-11-17 · AI: gpt-test\n",
            "\n",
            "1. The \"Comma\", Band — Song One (First, Album, 2020)\n",
            "   Because you like Seed Artist\n",
            "   https://musicbrainz.org/recording/11111111-1111-1111-1111-111111111111\n",
            "\n",
            "2. Beta Artist — Song \"Two\"\n",
            "   A deeper cut\n",
            "   https://musicbrainz.org/recording/22222222-2222-2222-2222-222222222222\n",
        );

        assert_eq!(to_plain_text(&sample()), expected);
    }

    #[test]
    fn m3u_contains_playlist_header_and_recording_urls() {
        let expected = concat!(
            "#EXTM3U\n",
            "#PLAYLIST:lbreco for alice\n",
            "#EXTINF:-1,The \"Comma\", Band - Song One\n",
            "https://musicbrainz.org/recording/11111111-1111-1111-1111-111111111111\n",
            "#EXTINF:-1,Beta Artist - Song \"Two\"\n",
            "https://musicbrainz.org/recording/22222222-2222-2222-2222-222222222222\n",
        );

        assert_eq!(to_m3u(&sample()), expected);
    }

    #[test]
    fn jspf_parses_back_with_expected_fields() {
        let text = to_jspf(&sample()).expect("JSPF should serialize");
        let value: serde_json::Value = serde_json::from_str(&text).expect("JSPF should parse");

        let playlist = value.get("playlist").expect("playlist object");
        assert_eq!(playlist["title"], "lbreco for alice — 2026-11-17");
        assert_eq!(playlist["creator"], "lbreco");
        assert_eq!(playlist["date"], "2026-11-17T12:34:56Z");

        let tracks = playlist["track"].as_array().expect("track array");
        assert_eq!(tracks.len(), 2);

        assert_eq!(
            tracks[0]["identifier"],
            "https://musicbrainz.org/recording/11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(tracks[0]["title"], "Song One");
        assert_eq!(tracks[0]["creator"], "The \"Comma\", Band");
        assert_eq!(tracks[0]["album"], "First, Album");
        assert_eq!(tracks[0]["duration"], 215_000);

        assert!(tracks[1].get("album").is_none());
        assert!(tracks[1].get("duration").is_none());
        assert_eq!(tracks[1]["creator"], "Beta Artist");
    }
}
