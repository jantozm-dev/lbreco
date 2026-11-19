use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};

use crate::lb::{LbClient, RadioMode};
use crate::model::{
    Candidate, CandidateSource, DiscoveryConfig, DiscoveryMode, TasteProfile, normalize_key,
};

const MAX_SEEDS: usize = 8;
const MAX_GENRES: usize = 3;
const CF_LIMIT: usize = 600;
const MAX_POOL: usize = 300;
/// Wider similar-artist fan-out per seed, and fewer tracks per artist, keeps
/// the pool broad enough that a "max 2 per artist" playlist can still reach 50.
const RADIO_SIMILAR_ARTISTS: u8 = 8;
const RADIO_RECORDINGS_PER_ARTIST: u8 = 2;
/// How many candidates from the same artist may survive the pool cap. The final
/// curator caps at 2, so 3 keeps a spare while still broadening the pool.
const MAX_PER_ARTIST_IN_POOL: usize = 3;
const MODE_CYCLE: [RadioMode; 3] = [RadioMode::Easy, RadioMode::Medium, RadioMode::Hard];

#[derive(Debug, Default)]
pub struct CandidatePool {
    pub candidates: Vec<Candidate>,
    pub warnings: Vec<String>,
}

/// A candidate before the metadata lookup, where we only know the MBID and the
/// source it came from.
#[derive(Debug)]
struct RawCandidate {
    mbid: String,
    source: CandidateSource,
    seed_artist: Option<String>,
    similar_artist: Option<String>,
    popularity: u64,
    cf_score: f64,
}

/// Build and enrich a pool of real recording candidates for this profile.
pub async fn build_candidates(
    client: &LbClient,
    profile: &TasteProfile,
    discovery: &DiscoveryConfig,
    history: &HashSet<String>,
    progress: &mut dyn FnMut(f64, String),
) -> Result<CandidatePool> {
    let mut report = |fraction: f64, message: &str| {
        progress(fraction.clamp(0.0, 1.0), message.to_string());
    };

    let mut warnings = Vec::new();
    let mut raw: HashMap<String, RawCandidate> = HashMap::new();

    report(0.01, "Checking your recommendation feedback");
    let exclusions = match client.exclusions().await {
        Ok(exclusions) => exclusions,
        Err(error) => {
            warnings.push(format!(
                "Could not read your disliked or do-not-recommend tracks: {error}"
            ));
            Default::default()
        }
    };

    let seeds: Vec<(String, String)> = profile
        .artists
        .iter()
        .filter_map(|artist| {
            let mbid = artist.mbid.as_deref()?.trim();
            if mbid.is_empty() || artist.name.trim().is_empty() {
                return None;
            }
            Some((artist.name.clone(), mbid.to_string()))
        })
        .take(MAX_SEEDS)
        .collect();

    if seeds.is_empty() {
        warnings.push("No top artists with MusicBrainz IDs; using other sources.".to_string());
    }

    for (index, (name, mbid)) in seeds.iter().enumerate() {
        let fraction = 0.04 + 0.40 * (index as f64 / seeds.len() as f64);
        report(fraction, &format!("Exploring music near {name}"));

        let mode = mode_for_seed(discovery.mode, index);
        match client
            .radio_artist(
                mbid,
                mode,
                RADIO_SIMILAR_ARTISTS,
                RADIO_RECORDINGS_PER_ARTIST,
            )
            .await
        {
            Ok(tracks) => {
                if tracks.is_empty() {
                    warnings.push(format!("Radio seed for {name} returned no tracks."));
                }
                for track in tracks {
                    if track.recording_mbid.trim().is_empty()
                        || exclusions.blocks_recording(&track.recording_mbid)
                    {
                        continue;
                    }
                    offer_raw(
                        &mut raw,
                        RawCandidate {
                            mbid: track.recording_mbid,
                            source: source_for_mode(mode),
                            seed_artist: Some(name.clone()),
                            similar_artist: track.similar_artist_name,
                            popularity: track.total_listen_count,
                            cf_score: 0.0,
                        },
                    );
                }
            }
            Err(error) => warnings.push(format!("Radio seed for {name} failed: {error}")),
        }
    }

    if discovery.include_genres {
        report(0.45, "Exploring your genres");

        let genres: Vec<String> = profile
            .genres
            .iter()
            .map(|genre| genre.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .take(MAX_GENRES)
            .collect();

        if genres.is_empty() {
            warnings.push("No genre stats available; genre exploration was skipped.".to_string());
        }

        for (index, genre) in genres.iter().enumerate() {
            let fraction = 0.46 + 0.10 * (index as f64 / genres.len() as f64);
            report(fraction, &format!("Exploring the {genre} genre"));

            match client.radio_tags(genre, 20).await {
                Ok(tracks) => {
                    if tracks.is_empty() {
                        warnings.push(format!("Genre {genre} returned no tracks."));
                    }
                    for track in tracks {
                        if track.recording_mbid.trim().is_empty()
                            || exclusions.blocks_recording(&track.recording_mbid)
                        {
                            continue;
                        }
                        offer_raw(
                            &mut raw,
                            RawCandidate {
                                mbid: track.recording_mbid,
                                source: CandidateSource::Genre,
                                seed_artist: Some(format!("Genre: {genre}")),
                                similar_artist: None,
                                popularity: 0,
                                cf_score: 0.0,
                            },
                        );
                    }
                }
                Err(error) => warnings.push(format!("Genre {genre} failed: {error}")),
            }
        }
    }

    report(0.58, "Finding listeners like you");
    match client.cf_recommendations(CF_LIMIT).await {
        Ok(items) => {
            let mut fresh = 0;
            let total = items.len();
            for item in items {
                // Weekly Exploration only keeps tracks the listener has never
                // played; a track with any listen timestamp is not new to them.
                if item.latest_listened_at.is_some()
                    || item.recording_mbid.trim().is_empty()
                    || exclusions.blocks_recording(&item.recording_mbid)
                {
                    continue;
                }
                fresh += 1;
                offer_raw(
                    &mut raw,
                    RawCandidate {
                        mbid: item.recording_mbid,
                        source: CandidateSource::Collaborative,
                        seed_artist: None,
                        similar_artist: None,
                        popularity: 0,
                        cf_score: item.score,
                    },
                );
            }
            if fresh == 0 {
                warnings.push(format!(
                    "None of the {total} collaborative recommendations were fresh enough; using the other sources."
                ));
            }
        }
        Err(error) => warnings.push(format!("Collaborative recommendations failed: {error}")),
    }

    // Tracks the user already knows are not candidates, no matter the source.
    raw.retain(|mbid, _| {
        !profile.known_mbids.contains(mbid)
            && !history.contains(mbid)
            && !exclusions.blocks_recording(mbid)
    });

    if raw.is_empty() {
        bail!("No candidate tracks found. Try again or adjust your discovery settings.");
    }

    let mut mbids: Vec<String> = raw.keys().cloned().collect();
    mbids.sort_unstable();

    report(
        0.66,
        &format!("Fetching details for {} tracks", mbids.len()),
    );
    let metadata = client
        .bulk_metadata(&mbids)
        .await
        .context("fetching recording metadata from ListenBrainz")?;

    let mut built = Vec::with_capacity(raw.len());
    for (mbid, raw_candidate) in raw {
        let Some(meta) = metadata.get(&mbid) else {
            continue;
        };
        if meta.title.trim().is_empty() || meta.artist.trim().is_empty() {
            continue;
        }
        if profile
            .known_keys
            .contains(&normalize_key(&meta.artist, &meta.title))
        {
            continue;
        }
        if exclusions.blocks_metadata(meta) {
            continue;
        }

        built.push(Candidate {
            id: String::new(),
            mbid,
            artist: meta.artist.clone(),
            title: meta.title.clone(),
            album: meta.album.clone(),
            year: meta.year,
            duration_ms: meta.duration_ms,
            tags: meta.tags.clone(),
            source: raw_candidate.source,
            seed_artist: raw_candidate.seed_artist,
            similar_artist: raw_candidate.similar_artist,
            popularity: raw_candidate.popularity,
            cf_score: raw_candidate.cf_score,
            first_release_date: meta.first_release_date.clone(),
        });
    }

    if built.is_empty() {
        bail!("No candidate tracks found. Try again or adjust your discovery settings.");
    }

    report(0.90, "Removing duplicate tracks");
    let mut deduped = dedupe_candidates(built);

    report(0.94, "Normalising collaborative scores");
    normalize_cf_scores(&mut deduped);

    report(0.96, "Ranking the pool");
    let candidates = rank_and_cap(deduped, MAX_POOL);

    if candidates.is_empty() {
        bail!("No candidate tracks found. Try again or adjust your discovery settings.");
    }

    report(1.0, &format!("Selected {} candidates", candidates.len()));

    Ok(CandidatePool {
        candidates,
        warnings,
    })
}

fn mode_for_seed(mode: DiscoveryMode, seed_index: usize) -> RadioMode {
    match mode {
        DiscoveryMode::Mixed => MODE_CYCLE[seed_index % MODE_CYCLE.len()],
        DiscoveryMode::Easy => RadioMode::Easy,
        DiscoveryMode::Medium => RadioMode::Medium,
        DiscoveryMode::Hard => RadioMode::Hard,
    }
}

fn source_for_mode(mode: RadioMode) -> CandidateSource {
    match mode {
        RadioMode::Easy => CandidateSource::RadioEasy,
        RadioMode::Medium => CandidateSource::RadioMedium,
        RadioMode::Hard => CandidateSource::RadioHard,
    }
}

fn offer_raw(pool: &mut HashMap<String, RawCandidate>, candidate: RawCandidate) {
    match pool.get_mut(&candidate.mbid) {
        Some(existing) => {
            if beats(
                candidate.source,
                candidate.popularity,
                candidate.cf_score,
                existing.source,
                existing.popularity,
                existing.cf_score,
            ) {
                *existing = candidate;
            }
        }
        None => {
            pool.insert(candidate.mbid.clone(), candidate);
        }
    }
}

/// Compare candidates by source strength first, then popularity and
/// collaborative score. Used both while merging raw sources and after
/// enrichment.
fn beats(
    source: CandidateSource,
    popularity: u64,
    cf_score: f64,
    other_source: CandidateSource,
    other_popularity: u64,
    other_cf_score: f64,
) -> bool {
    let weight = source.weight();
    let other_weight = other_source.weight();
    if (weight - other_weight).abs() > f64::EPSILON {
        return weight > other_weight;
    }
    if popularity != other_popularity {
        return popularity > other_popularity;
    }
    cf_score > other_cf_score
}

fn candidate_is_better(candidate: &Candidate, existing: &Candidate) -> bool {
    beats(
        candidate.source,
        candidate.popularity,
        candidate.cf_score,
        existing.source,
        existing.popularity,
        existing.cf_score,
    )
}

fn keep_better(pool: &mut HashMap<String, Candidate>, key: String, candidate: Candidate) {
    match pool.get_mut(&key) {
        Some(existing) => {
            if candidate_is_better(&candidate, existing) {
                *existing = candidate;
            }
        }
        None => {
            pool.insert(key, candidate);
        }
    }
}

/// Deduplicate by recording MBID and then by normalized artist|title, keeping
/// the stronger source on collisions.
fn dedupe_candidates(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut by_mbid: HashMap<String, Candidate> = HashMap::new();
    for candidate in candidates {
        let key = candidate.mbid.clone();
        keep_better(&mut by_mbid, key, candidate);
    }

    let mut by_key: HashMap<String, Candidate> = HashMap::new();
    for candidate in by_mbid.into_values() {
        let key = candidate.key();
        keep_better(&mut by_key, key, candidate);
    }

    by_key.into_values().collect()
}

/// ListenBrainz's raw CF scores sit around 0.06, so using them directly makes
/// the strongest collaborative recommendation nearly weightless. Scale the
/// pool's best score to 1.0 before ranking.
fn normalize_cf_scores(candidates: &mut [Candidate]) {
    let max = candidates
        .iter()
        .map(|candidate| candidate.cf_score.max(0.0))
        .fold(0.0f64, f64::max);
    if max <= f64::EPSILON {
        return;
    }
    for candidate in candidates {
        candidate.cf_score = (candidate.cf_score.max(0.0) / max).clamp(0.0, 1.0);
    }
}

/// Pre-curation score: the source weight leads, while popularity, collaborative
/// score and a stable per-track jitter separate equal-weight candidates.
fn pre_score(candidate: &Candidate) -> f64 {
    let popularity = (1.0 + candidate.popularity as f64).ln();
    let cf_score = candidate.cf_score.clamp(-1.0, 1.0);
    candidate.source.weight() * 2.0
        + popularity * 0.12
        + cf_score * 0.5
        + stable_jitter(&candidate.mbid) * 0.04
}

/// FNV-1a based jitter in 0.0..1.0. Unlike a random generator this is stable
/// between runs, so the same pool always ranks the same way.
fn stable_jitter(mbid: &str) -> f64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in mbid.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % 10_000) as f64 / 10_000.0
}

fn rank_and_cap(candidates: Vec<Candidate>, limit: usize) -> Vec<Candidate> {
    let mut scored: Vec<(f64, Candidate)> = candidates
        .into_iter()
        .map(|candidate| (pre_score(&candidate), candidate))
        .collect();

    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.mbid.cmp(&b.1.mbid)));

    // First pass: prefer breadth, so one very popular artist cannot eat the
    // whole pool. Second pass: top up with whatever is left, in the same order.
    let mut per_artist: HashMap<String, usize> = HashMap::new();
    let mut taken = vec![false; scored.len()];
    let mut chosen: Vec<Candidate> = Vec::new();

    for (index, (_, candidate)) in scored.iter().enumerate() {
        if chosen.len() >= limit {
            break;
        }
        let count = per_artist.entry(artist_key(candidate)).or_insert(0);
        if *count < MAX_PER_ARTIST_IN_POOL {
            *count += 1;
            taken[index] = true;
            chosen.push(candidate.clone());
        }
    }

    if chosen.len() < limit {
        for (index, (_, candidate)) in scored.iter().enumerate() {
            if chosen.len() >= limit {
                break;
            }
            if taken[index] {
                continue;
            }
            chosen.push(candidate.clone());
        }
    }

    chosen
        .into_iter()
        .enumerate()
        .map(|(index, mut candidate)| {
            candidate.id = format!("c{:03}", index + 1);
            candidate
        })
        .collect()
}

fn artist_key(candidate: &Candidate) -> String {
    candidate
        .artist
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(mbid: &str, artist: &str, title: &str, source: CandidateSource) -> Candidate {
        Candidate {
            mbid: mbid.to_string(),
            artist: artist.to_string(),
            title: title.to_string(),
            source,
            ..Candidate::default()
        }
    }

    #[test]
    fn mixed_mode_cycles_through_radio_modes() {
        let modes: Vec<RadioMode> = (0..6)
            .map(|index| mode_for_seed(DiscoveryMode::Mixed, index))
            .collect();
        assert_eq!(
            modes,
            vec![
                RadioMode::Easy,
                RadioMode::Medium,
                RadioMode::Hard,
                RadioMode::Easy,
                RadioMode::Medium,
                RadioMode::Hard,
            ]
        );
    }

    #[test]
    fn configured_mode_applies_to_every_seed() {
        for index in 0..MAX_SEEDS {
            assert_eq!(mode_for_seed(DiscoveryMode::Easy, index), RadioMode::Easy);
            assert_eq!(
                mode_for_seed(DiscoveryMode::Medium, index),
                RadioMode::Medium
            );
            assert_eq!(mode_for_seed(DiscoveryMode::Hard, index), RadioMode::Hard);
        }
    }

    #[test]
    fn dedupe_keeps_the_stronger_source() {
        let result = dedupe_candidates(vec![
            candidate(
                "mbid-easy",
                "The Artist",
                "Song!",
                CandidateSource::RadioEasy,
            ),
            candidate(
                "mbid-hard",
                "the artist",
                "song",
                CandidateSource::RadioHard,
            ),
        ]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].mbid, "mbid-hard");
        assert_eq!(result[0].source, CandidateSource::RadioHard);
    }

    #[test]
    fn dedupe_by_mbid_keeps_the_better_variant() {
        let result = dedupe_candidates(vec![
            candidate("same", "Artist", "Song", CandidateSource::Genre),
            candidate(
                "same",
                "Artist",
                "Song (Remastered)",
                CandidateSource::Collaborative,
            ),
        ]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "Song (Remastered)");
        assert_eq!(result[0].source, CandidateSource::Collaborative);
    }

    #[test]
    fn dedupe_breaks_source_ties_by_popularity_and_cf_score() {
        let mut popular = candidate("popular", "Artist", "Song", CandidateSource::RadioMedium);
        popular.popularity = 50;
        let mut obscure = candidate("obscure", "Artist", "Song", CandidateSource::RadioMedium);
        obscure.popularity = 5;
        let result = dedupe_candidates(vec![obscure, popular]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].mbid, "popular");

        let mut low_cf = candidate("low-cf", "Artist", "Song", CandidateSource::RadioMedium);
        low_cf.cf_score = 0.2;
        let mut high_cf = candidate("high-cf", "Artist", "Song", CandidateSource::RadioMedium);
        high_cf.cf_score = 0.9;
        let result = dedupe_candidates(vec![low_cf, high_cf]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].mbid, "high-cf");
    }

    #[test]
    fn cap_orders_by_pre_score_and_assigns_sequential_ids() {
        let mut pool = vec![
            candidate("hard", "Artist", "Hard", CandidateSource::RadioHard),
            candidate("easy", "Artist", "Easy", CandidateSource::RadioEasy),
        ];
        pool[0].popularity = 20;
        pool[1].popularity = 1_000;
        for index in 0..303 {
            pool.push(candidate(
                &format!("filler-{index:03}"),
                "Filler",
                &format!("Filler {index}"),
                CandidateSource::Genre,
            ));
        }

        let ranked = rank_and_cap(pool, MAX_POOL);
        assert_eq!(ranked.len(), MAX_POOL);
        assert!(ranked.iter().any(|item| item.mbid == "hard"));
        assert!(ranked.iter().any(|item| item.mbid == "easy"));
        for (index, item) in ranked.iter().enumerate() {
            assert_eq!(item.id, format!("c{:03}", index + 1));
        }

        let hard_rank = ranked.iter().position(|item| item.mbid == "hard").unwrap();
        let easy_rank = ranked.iter().position(|item| item.mbid == "easy").unwrap();
        assert!(hard_rank < easy_rank);
    }

    #[test]
    fn cap_order_is_stable_between_runs() {
        let pool: Vec<Candidate> = (0..20)
            .map(|index| {
                candidate(
                    &format!("mbid-{index:02}"),
                    "Artist",
                    &format!("Track {index}"),
                    CandidateSource::Genre,
                )
            })
            .collect();

        let first = rank_and_cap(pool.clone(), MAX_POOL);
        let second = rank_and_cap(pool, MAX_POOL);
        let first_order: Vec<&str> = first.iter().map(|item| item.mbid.as_str()).collect();
        let second_order: Vec<&str> = second.iter().map(|item| item.mbid.as_str()).collect();
        assert_eq!(first_order, second_order);
    }

    #[test]
    fn cf_scores_are_normalised_to_the_pool_maximum() {
        let mut candidates = vec![
            candidate("a", "A", "One", CandidateSource::Collaborative),
            candidate("b", "B", "Two", CandidateSource::Collaborative),
            candidate("c", "C", "Three", CandidateSource::RadioEasy),
        ];
        candidates[0].cf_score = 0.06;
        candidates[1].cf_score = 0.03;
        normalize_cf_scores(&mut candidates);
        assert!((candidates[0].cf_score - 1.0).abs() < 1e-9);
        assert!((candidates[1].cf_score - 0.5).abs() < 1e-9);
        assert!((candidates[2].cf_score - 0.0).abs() < 1e-9);
    }
}
