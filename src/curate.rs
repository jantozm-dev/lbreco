use std::collections::{HashMap, HashSet};

use crate::model::{Candidate, CandidateSource, DiscoveryMode, Recommendation, TasteProfile};

const MAX_PER_ARTIST: usize = 2;
const MAX_PER_ALBUM: usize = 2;
const MAX_PER_DECADE: usize = 10;
const MAX_REASON_CHARS: usize = 160;
/// Score given to a candidate the AI model ranked, so it is always considered
/// before the deterministic score order.
const PRIORITY_SCORE: f64 = 1000.0;

/// Merge AI picks with deterministic picks, validating ids and diversity caps.
pub fn build_recommendations(
    profile: &TasteProfile,
    candidates: &[Candidate],
    ai_picks: &[(String, String)],
    count: usize,
    mode: DiscoveryMode,
) -> Vec<Recommendation> {
    if count == 0 {
        return Vec::new();
    }

    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let id = candidate.id.trim();
        if !id.is_empty() {
            by_id.entry(id).or_insert(index);
        }
    }

    let mut state = SelectionState::default();
    let mut chosen: Vec<(usize, String)> = Vec::new();

    // The model's ranking, restricted to ids that really exist. Unknown ids are
    // dropped so a hallucinated track can never enter the playlist.
    let mut priority: Vec<(usize, String)> = Vec::new();
    for (id, reason) in ai_picks {
        let Some(&index) = by_id.get(id.trim()) else {
            continue;
        };
        let candidate = &candidates[index];
        let provided = reason.trim();
        let reason = if provided.is_empty() {
            reason_for(profile, candidate)
        } else {
            clip_reason(provided)
        };
        priority.push((index, reason));
    }

    // Keep the model's order, but reject ids that are already excluded by the
    // diversity caps.
    for (index, reason) in &priority {
        if chosen.len() >= count {
            break;
        }
        let candidate = &candidates[*index];
        if !state.allows(candidate) {
            continue;
        }

        state.add(candidate);
        chosen.push((*index, reason.clone()));
    }

    if chosen.len() < count {
        let scores = base_scores(profile, candidates);
        let mut filled = Vec::new();
        greedy_fill(
            candidates,
            &scores,
            &priority,
            count,
            mode,
            &mut state,
            &mut filled,
        );
        for (index, reason) in filled {
            let reason = reason.unwrap_or_else(|| reason_for(profile, &candidates[index]));
            chosen.push((index, reason));
        }
    }

    chosen
        .into_iter()
        .enumerate()
        .map(|(position, (index, reason))| {
            let mut recommendation = Recommendation::from(&candidates[index]);
            recommendation.rank = position + 1;
            recommendation.reason = if reason.trim().is_empty() {
                reason_for(profile, &candidates[index])
            } else {
                reason
            };
            recommendation
        })
        .collect()
}

/// Pure deterministic fallback.
pub fn deterministic_recommendations(
    profile: &TasteProfile,
    candidates: &[Candidate],
    count: usize,
    mode: DiscoveryMode,
) -> Vec<Recommendation> {
    if count == 0 {
        return Vec::new();
    }

    let scores = base_scores(profile, candidates);
    let mut state = SelectionState::default();
    let mut chosen = Vec::new();
    greedy_fill(
        candidates,
        &scores,
        &[],
        count,
        mode,
        &mut state,
        &mut chosen,
    );

    chosen
        .into_iter()
        .enumerate()
        .map(|(position, (index, _))| {
            let candidate = &candidates[index];
            let mut recommendation = Recommendation::from(candidate);
            recommendation.rank = position + 1;
            recommendation.reason = reason_for(profile, candidate);
            recommendation
        })
        .collect()
}

pub fn reason_for(_profile: &TasteProfile, candidate: &Candidate) -> String {
    let tag = candidate
        .tags
        .iter()
        .map(|tag| tag.trim())
        .find(|tag| !tag.is_empty())
        .map(|tag| tag.to_lowercase());

    let seed = candidate
        .seed_artist
        .as_deref()
        .map(str::trim)
        .filter(|seed| !seed.is_empty());

    let similar = candidate
        .similar_artist
        .as_deref()
        .map(str::trim)
        .filter(|similar| !similar.is_empty());

    let reason = if let Some(seed) = seed {
        if let Some(genre) = seed
            .strip_prefix("Genre:")
            .map(str::trim)
            .filter(|genre| !genre.is_empty())
        {
            match &tag {
                Some(tag) => format!(
                    "A {genre} detour with a {tag} edge, picked to widen your usual rotation."
                ),
                None => format!("A {genre} detour picked to widen your usual rotation."),
            }
        } else {
            match &tag {
                Some(tag) => format!(
                    "Because {seed} is in your rotation, this {tag} track sits nearby without repeating it."
                ),
                None => format!(
                    "Because {seed} is in your rotation, this sits nearby without repeating it."
                ),
            }
        }
    } else if let Some(similar) = similar {
        match &tag {
            Some(tag) => {
                format!("A {tag} track in the same lane as {similar}, but new to your history.")
            }
            None => format!("In the same lane as {similar}, but new to your history."),
        }
    } else {
        match candidate.source {
            CandidateSource::Collaborative => match &tag {
                Some(tag) => format!(
                    "A {tag} track that listeners with overlapping taste keep coming back to."
                ),
                None => "Listeners with overlapping taste keep coming back to this.".to_string(),
            },
            CandidateSource::RadioHard => match &tag {
                Some(tag) => {
                    format!("A deeper {tag} cut for going beyond your usual top artists.")
                }
                None => "A deeper cut for going beyond your usual top artists.".to_string(),
            },
            CandidateSource::RadioMedium => match &tag {
                Some(tag) => {
                    format!("A {tag} step out from the artists you already play.")
                }
                None => "A small step out from the artists you already play.".to_string(),
            },
            CandidateSource::RadioEasy => match &tag {
                Some(tag) => {
                    format!("A familiar-adjacent {tag} pick that should fit right in.")
                }
                None => "A familiar-adjacent pick that should fit right in.".to_string(),
            },
            CandidateSource::Genre => match &tag {
                Some(tag) => format!("A {tag} pick to stretch your listening a little."),
                None => "A genre-led pick to stretch your listening a little.".to_string(),
            },
        }
    };

    clip_reason(&reason)
}

/// Means "no limit at all".
const OPEN: usize = usize::MAX;

/// The diversity caps the curator works under, as `(per artist, per album, per
/// decade)`. The first entry is the strict one; the later entries are used one
/// after the other, and only when the stricter caps cannot fill the requested
/// number of tracks. The last entry allows everything, so the app can always
/// deliver the number of tracks it promised.
const LIMIT_STAGES: [(usize, usize, usize); 8] = [
    (MAX_PER_ARTIST, MAX_PER_ALBUM, MAX_PER_DECADE),
    (3, 3, MAX_PER_DECADE),
    (3, 3, 20),
    (4, 4, 20),
    (4, 4, 30),
    (6, 6, 40),
    (OPEN, OPEN, 60),
    (OPEN, OPEN, OPEN),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    per_artist: usize,
    per_album: usize,
    per_decade: usize,
    stage: usize,
}

impl Default for Limits {
    fn default() -> Self {
        let (per_artist, per_album, per_decade) = LIMIT_STAGES[0];
        Self {
            per_artist,
            per_album,
            per_decade,
            stage: 0,
        }
    }
}

impl Limits {
    /// The next, looser set of caps.
    fn relaxed(self) -> Self {
        let Some(stage) = self
            .stage
            .checked_add(1)
            .filter(|stage| *stage < LIMIT_STAGES.len())
        else {
            return self;
        };
        let (per_artist, per_album, per_decade) = LIMIT_STAGES[stage];
        Self {
            per_artist,
            per_album,
            per_decade,
            stage,
        }
    }

    /// True once no cap is left, so relaxing further cannot help.
    fn is_open(self) -> bool {
        self.stage + 1 >= LIMIT_STAGES.len()
    }
}

#[derive(Debug, Default)]
struct SelectionState {
    limits: Limits,
    artist_counts: HashMap<String, usize>,
    album_counts: HashMap<String, usize>,
    decade_counts: HashMap<i32, usize>,
    tag_counts: HashMap<String, usize>,
    used_records: HashSet<String>,
    picks: usize,
    exploratory: usize,
}

impl SelectionState {
    fn allows(&self, candidate: &Candidate) -> bool {
        if self.used_records.contains(&record_key(candidate)) {
            return false;
        }
        let artist = artist_key(&candidate.artist);
        if self.artist_counts.get(&artist).copied().unwrap_or(0) >= self.limits.per_artist {
            return false;
        }
        if let Some(album) = album_key(candidate) {
            if self.album_counts.get(&album).copied().unwrap_or(0) >= self.limits.per_album {
                return false;
            }
        }
        if self
            .decade_counts
            .get(&decade_of(candidate))
            .copied()
            .unwrap_or(0)
            >= self.limits.per_decade
        {
            return false;
        }
        true
    }

    fn add(&mut self, candidate: &Candidate) {
        self.picks += 1;
        *self
            .artist_counts
            .entry(artist_key(&candidate.artist))
            .or_default() += 1;
        if let Some(album) = album_key(candidate) {
            *self.album_counts.entry(album).or_default() += 1;
        }
        *self.decade_counts.entry(decade_of(candidate)).or_default() += 1;
        if let Some(tag) = top_tag(candidate) {
            *self.tag_counts.entry(tag).or_default() += 1;
        }
        self.used_records.insert(record_key(candidate));
        if candidate.source.is_exploratory() {
            self.exploratory += 1;
        }
    }
}

/// Fill up to `target` picks. `priority` holds the candidates the AI model
/// ranked first, in the model's order, together with the reason it gave. The
/// remaining candidates are considered by score.
///
/// When nothing fits under the current diversity caps, the caps are widened one
/// step and the search continues. Without that, a narrow pool (few artists, a
/// handful of decades) would leave the app short of the number of tracks it
/// promised.
fn greedy_fill(
    candidates: &[Candidate],
    scores: &[f64],
    priority: &[(usize, String)],
    target: usize,
    mode: DiscoveryMode,
    state: &mut SelectionState,
    chosen: &mut Vec<(usize, Option<String>)>,
) {
    // Aim for the mode's share of exploratory picks, but only start steering
    // once 60% of the slots have been filled.
    let exploratory_quota = exploratory_quota(target, mode);

    while state.picks < target {
        let boosting = state.picks.saturating_mul(5) >= target.saturating_mul(3)
            && state.exploratory < exploratory_quota;

        let mut best: Option<(usize, f64, Option<String>)> = None;

        // Picks the model ranked first keep priority over the score order.
        for (position, (index, reason)) in priority.iter().enumerate() {
            let candidate = &candidates[*index];
            if !state.allows(candidate) {
                continue;
            }
            let score = PRIORITY_SCORE - position as f64;
            if best
                .as_ref()
                .is_none_or(|(_, best_score, _)| score > *best_score)
            {
                best = Some((*index, score, Some(reason.clone())));
            }
        }

        for (index, candidate) in candidates.iter().enumerate() {
            if !state.allows(candidate) {
                continue;
            }

            let mut score = scores.get(index).copied().unwrap_or(0.0);
            if boosting && candidate.source.is_exploratory() {
                score += 0.55;
            }
            if let Some(tag) = top_tag(candidate) {
                let reused = state.tag_counts.get(&tag).copied().unwrap_or(0);
                score -= (reused as f64 * 0.05).min(0.25);
            }

            if best
                .as_ref()
                .is_none_or(|(_, best_score, _)| score > *best_score)
            {
                best = Some((index, score, None));
            }
        }

        let Some((index, _, reason)) = best else {
            // Nothing fits under the current caps: widen them and try again.
            if state.limits.is_open() {
                break;
            }
            state.limits = state.limits.relaxed();
            continue;
        };

        state.add(&candidates[index]);
        chosen.push((index, reason));
    }
}

/// How many of the requested tracks should come from exploratory sources.
/// ListenBrainz's Weekly Exploration is entirely new music; the easier modes
/// deliberately stay closer to the listener's lane.
fn exploratory_quota(target: usize, mode: DiscoveryMode) -> usize {
    let percent = match mode {
        DiscoveryMode::Easy => 20,
        DiscoveryMode::Medium => 40,
        DiscoveryMode::Hard => 65,
        DiscoveryMode::Mixed => 40,
    };
    (target.saturating_mul(percent) + 99) / 100
}

fn base_scores(profile: &TasteProfile, candidates: &[Candidate]) -> Vec<f64> {
    let ranks = artist_ranks(profile);
    candidates
        .iter()
        .map(|candidate| base_score(candidate, &ranks))
        .collect()
}

fn base_score(candidate: &Candidate, artist_ranks: &HashMap<String, usize>) -> f64 {
    let popularity = ((1.0 + candidate.popularity as f64).ln() / 1001f64.ln()).clamp(0.0, 1.0);
    let cf_score = candidate.cf_score.clamp(0.0, 1.0);
    let novelty = match artist_ranks.get(&artist_key(&candidate.artist)) {
        // Ranks 1-20 are already familiar.
        Some(&rank) if rank < 20 => 0.0,
        // Ranks 21-50 are a half-step away.
        Some(&rank) if rank < 50 => 0.5,
        // Everything else (including artists we have never seen) is novel.
        _ => 1.0,
    } + if candidate.source.is_exploratory() {
        0.15
    } else {
        0.0
    };

    0.42 * candidate.source.weight()
        + 0.20 * popularity
        + 0.18 * cf_score
        + 0.20 * novelty
        + jitter(candidate)
}

fn artist_ranks(profile: &TasteProfile) -> HashMap<String, usize> {
    let mut ranks = HashMap::new();
    for (index, artist) in profile.artists.iter().enumerate() {
        let key = artist_key(&artist.name);
        if !key.is_empty() {
            ranks.entry(key).or_insert(index);
        }
    }
    ranks
}

fn jitter(candidate: &Candidate) -> f64 {
    // FNV-1a over the recording identity gives a stable, tiny tie-breaker
    // without pulling in a random number generator.
    let source = if candidate.mbid.trim().is_empty() {
        candidate.key()
    } else {
        candidate.mbid.clone()
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % 10_007) as f64 / 100_070_000.0
}

fn artist_key(value: &str) -> String {
    value.trim().to_lowercase()
}

fn album_key(candidate: &Candidate) -> Option<String> {
    candidate
        .album
        .as_deref()
        .map(str::trim)
        .filter(|album| !album.is_empty())
        .map(str::to_lowercase)
}

fn decade_of(candidate: &Candidate) -> i32 {
    candidate
        .year
        .map(|year| year.div_euclid(10) * 10)
        .unwrap_or(i32::MIN)
}

fn top_tag(candidate: &Candidate) -> Option<String> {
    candidate
        .tags
        .iter()
        .map(|tag| tag.trim())
        .find(|tag| !tag.is_empty())
        .map(str::to_lowercase)
}

fn record_key(candidate: &Candidate) -> String {
    let mbid = candidate.mbid.trim();
    if mbid.is_empty() {
        format!("key:{}", candidate.key())
    } else {
        format!("mbid:{mbid}")
    }
}

fn clip_reason(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_REASON_CHARS {
        return normalized;
    }

    let mut clipped: String = normalized.chars().take(MAX_REASON_CHARS).collect();
    // Prefer ending on a word boundary rather than mid-word.
    if let Some(index) = clipped.rfind(' ') {
        if index > 0 {
            clipped.truncate(index);
        }
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ArtistStat;

    fn candidate(
        id: &str,
        artist: &str,
        album: Option<&str>,
        year: Option<i32>,
        source: CandidateSource,
    ) -> Candidate {
        Candidate {
            id: id.to_string(),
            mbid: format!("mbid-{id}"),
            artist: artist.to_string(),
            title: format!("Title {id}"),
            album: album.map(str::to_string),
            year,
            source,
            popularity: 50,
            cf_score: 0.4,
            tags: vec!["test".to_string()],
            ..Candidate::default()
        }
    }

    fn profile() -> TasteProfile {
        TasteProfile {
            username: "tester".to_string(),
            artists: vec![ArtistStat {
                name: "Favorite".to_string(),
                listen_count: 100,
                ..ArtistStat::default()
            }],
            ..TasteProfile::default()
        }
    }

    #[test]
    fn deterministic_caps_two_per_artist() {
        let candidates = vec![
            candidate(
                "a1",
                "Repeat",
                Some("One"),
                Some(1999),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "a2",
                "Repeat",
                Some("Two"),
                Some(2001),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "a3",
                "Repeat",
                Some("Three"),
                Some(2003),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "a4",
                "Repeat",
                Some("Four"),
                Some(2005),
                CandidateSource::RadioEasy,
            ),
        ];
        // While the pool allows it, the strict cap of two per artist holds.
        let recommendations =
            deterministic_recommendations(&profile(), &candidates, 2, DiscoveryMode::Mixed);
        assert_eq!(recommendations.len(), 2);
        assert_eq!(
            recommendations
                .iter()
                .filter(|recommendation| recommendation.artist == "Repeat")
                .count(),
            2
        );
    }

    #[test]
    fn deterministic_caps_two_per_album() {
        let candidates = vec![
            candidate(
                "b1",
                "One",
                Some("Shared"),
                Some(1999),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "b2",
                "Two",
                Some("Shared"),
                Some(2001),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "b3",
                "Three",
                Some("Shared"),
                Some(2003),
                CandidateSource::RadioEasy,
            ),
        ];
        let recommendations =
            deterministic_recommendations(&profile(), &candidates, 2, DiscoveryMode::Mixed);
        assert_eq!(
            recommendations
                .iter()
                .filter(|recommendation| recommendation.album.as_deref() == Some("Shared"))
                .count(),
            2
        );
    }

    #[test]
    fn caps_relax_only_when_the_pool_is_too_narrow() {
        // Four tracks by one artist, one decade. The strict caps allow two of
        // them; asking for four has to widen the caps.
        let candidates = vec![
            candidate(
                "s1",
                "Solo",
                Some("One"),
                Some(2019),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "s2",
                "Solo",
                Some("Two"),
                Some(2020),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "s3",
                "Solo",
                Some("Three"),
                Some(2021),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "s4",
                "Solo",
                Some("Four"),
                Some(2022),
                CandidateSource::RadioEasy,
            ),
        ];

        let strict =
            deterministic_recommendations(&profile(), &candidates, 2, DiscoveryMode::Mixed);
        assert_eq!(strict.len(), 2);
        assert_eq!(
            strict
                .iter()
                .filter(|recommendation| recommendation.artist == "Solo")
                .count(),
            2
        );

        // A sparse pool still fills the requested count.
        let relaxed =
            deterministic_recommendations(&profile(), &candidates, 4, DiscoveryMode::Mixed);
        assert_eq!(relaxed.len(), 4);
        assert_eq!(
            relaxed
                .iter()
                .map(|recommendation| recommendation.rank)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert!(relaxed.iter().all(|item| !item.reason.trim().is_empty()));
    }

    #[test]
    fn sparse_pool_still_fills_the_requested_count() {
        // 60 tracks spread over only three artists: far too few for a strict
        // two-per-artist cap to ever reach 50.
        let candidates: Vec<Candidate> = (0..60)
            .map(|index| {
                let artist = format!("Artist {}", index % 3);
                let album = format!("Album {}", index / 3);
                candidate(
                    &format!("p{index}"),
                    &artist,
                    Some(&album),
                    Some(1990 + (index % 5) as i32),
                    CandidateSource::RadioMedium,
                )
            })
            .collect();

        let recommendations =
            deterministic_recommendations(&profile(), &candidates, 50, DiscoveryMode::Mixed);
        assert_eq!(recommendations.len(), 50);
        assert_eq!(
            recommendations
                .iter()
                .map(|recommendation| recommendation.rank)
                .collect::<Vec<_>>(),
            (1..=50).collect::<Vec<_>>()
        );
        // No duplicate tracks, and the three artists are all represented.
        let mut mbids: Vec<&str> = recommendations
            .iter()
            .map(|recommendation| recommendation.mbid.as_str())
            .collect();
        mbids.sort_unstable();
        mbids.dedup();
        assert_eq!(mbids.len(), 50);
        assert_eq!(
            recommendations
                .iter()
                .map(|recommendation| recommendation.artist.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn deterministic_returns_exact_count_and_sequential_ranks() {
        let candidates: Vec<Candidate> = (0..8)
            .map(|index| {
                candidate(
                    &format!("c{index}"),
                    &format!("Artist {index}"),
                    Some(&format!("Album {index}")),
                    Some(1990 + index as i32),
                    CandidateSource::RadioMedium,
                )
            })
            .collect();

        let recommendations =
            deterministic_recommendations(&profile(), &candidates, 5, DiscoveryMode::Mixed);
        assert_eq!(recommendations.len(), 5);
        assert_eq!(
            recommendations
                .iter()
                .map(|recommendation| recommendation.rank)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert!(
            recommendations
                .iter()
                .all(|recommendation| !recommendation.reason.trim().is_empty())
        );
        assert!(
            deterministic_recommendations(&profile(), &candidates, 0, DiscoveryMode::Mixed)
                .is_empty()
        );
        assert_eq!(
            deterministic_recommendations(&profile(), &candidates, 100, DiscoveryMode::Mixed).len(),
            candidates.len()
        );
    }

    #[test]
    fn build_recommendations_ignores_invalid_ai_ids() {
        let candidates = vec![
            candidate(
                "c1",
                "Alpha",
                Some("A"),
                Some(1990),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "c2",
                "Beta",
                Some("B"),
                Some(2000),
                CandidateSource::RadioMedium,
            ),
            candidate(
                "c3",
                "Gamma",
                Some("C"),
                Some(2010),
                CandidateSource::RadioHard,
            ),
        ];
        let ai_picks = vec![
            ("missing".to_string(), "not real".to_string()),
            ("c2".to_string(), "A valid model reason.".to_string()),
            ("c2".to_string(), "duplicate".to_string()),
            ("c1".to_string(), String::new()),
        ];

        let recommendations =
            build_recommendations(&profile(), &candidates, &ai_picks, 3, DiscoveryMode::Mixed);
        assert_eq!(recommendations.len(), 3);
        assert_eq!(
            recommendations
                .iter()
                .map(|recommendation| recommendation.rank)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(recommendations[0].title, "Title c2");
        assert_eq!(recommendations[0].reason, "A valid model reason.");
        assert_eq!(recommendations[1].title, "Title c1");
        assert!(recommendations.iter().all(|item| !item.reason.is_empty()));
    }

    #[test]
    fn build_recommendations_enforces_caps_across_ai_and_fallback() {
        let candidates = vec![
            candidate(
                "c1",
                "Repeat",
                Some("One"),
                Some(1990),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "c2",
                "Repeat",
                Some("Two"),
                Some(2000),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "c3",
                "Repeat",
                Some("Three"),
                Some(2010),
                CandidateSource::RadioEasy,
            ),
            candidate(
                "c4",
                "Other",
                Some("Four"),
                Some(2020),
                CandidateSource::RadioEasy,
            ),
        ];
        let ai_picks = vec![
            ("c1".to_string(), "First.".to_string()),
            ("c2".to_string(), "Second.".to_string()),
        ];
        // Three fits inside the strict caps: two from "Repeat" and one other.
        let recommendations =
            build_recommendations(&profile(), &candidates, &ai_picks, 3, DiscoveryMode::Mixed);
        assert_eq!(recommendations.len(), 3);
        assert_eq!(
            recommendations
                .iter()
                .filter(|recommendation| recommendation.artist == "Repeat")
                .count(),
            MAX_PER_ARTIST
        );

        // Asking for four needs a third "Repeat" track, so the caps widen.
        let relaxed =
            build_recommendations(&profile(), &candidates, &ai_picks, 4, DiscoveryMode::Mixed);
        assert_eq!(relaxed.len(), 4);
        assert_eq!(
            relaxed
                .iter()
                .filter(|recommendation| recommendation.artist == "Repeat")
                .count(),
            3
        );
        // The model's reasons survive the refill.
        assert_eq!(relaxed[0].reason, "First.");
        assert_eq!(relaxed[1].reason, "Second.");
    }

    #[test]
    fn reasons_are_non_empty_and_grounded() {
        let mut genre = candidate("g1", "A", None, None, CandidateSource::Genre);
        genre.seed_artist = Some("Genre: jazz".to_string());
        let reason = reason_for(&profile(), &genre);
        assert!(reason.contains("jazz"));
        assert!(reason.contains("detour"));

        let mut seeded = candidate("g2", "B", None, None, CandidateSource::RadioEasy);
        seeded.seed_artist = Some("Portishead".to_string());
        let reason = reason_for(&profile(), &seeded);
        assert!(reason.contains("Portishead"));

        let mut similar = candidate("g3", "C", None, None, CandidateSource::RadioMedium);
        similar.similar_artist = Some("Massive Attack".to_string());
        let reason = reason_for(&profile(), &similar);
        assert!(reason.contains("Massive Attack"));

        for source in [
            CandidateSource::RadioEasy,
            CandidateSource::RadioMedium,
            CandidateSource::RadioHard,
            CandidateSource::Genre,
            CandidateSource::Collaborative,
        ] {
            let item = candidate("r", "D", None, None, source);
            let reason = reason_for(&profile(), &item);
            assert!(!reason.trim().is_empty());
            let words = reason.split_whitespace().count();
            assert!((8..=18).contains(&words), "{source:?}: {reason}");
        }
    }

    #[test]
    fn exploratory_quota_follows_the_discovery_mode() {
        assert_eq!(exploratory_quota(50, DiscoveryMode::Easy), 10);
        assert_eq!(exploratory_quota(50, DiscoveryMode::Medium), 20);
        assert_eq!(exploratory_quota(50, DiscoveryMode::Hard), 33);
        assert_eq!(exploratory_quota(50, DiscoveryMode::Mixed), 20);
    }
}
