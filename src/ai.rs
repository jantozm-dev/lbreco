use std::collections::{HashMap, HashSet};
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

use crate::model::{AiConfig, Candidate, TasteProfile};

#[derive(Debug, Default)]
pub struct AiCuration {
    /// (candidate id, short reason)
    pub picks: Vec<(String, String)>,
    pub warning: Option<String>,
}

const MAX_PROMPT_CANDIDATES: usize = 220;
const MAX_REASON_CHARS: usize = 160;
/// Reasoning models can spend thousands of tokens thinking before they emit
/// the JSON answer. A 4k budget only produced an empty `content` field with
/// Nemotron, and 16k was sometimes cut off mid-array, so leave room for both
/// the reasoning and the picks.
const MAX_OUTPUT_TOKENS: u32 = 24_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Ask an OpenAI-compatible model to curate the candidate pool.
/// Failures are returned as a warning; the caller falls back to deterministic curation.
pub async fn curate(
    profile: &TasteProfile,
    candidates: &[Candidate],
    config: &AiConfig,
    count: usize,
) -> AiCuration {
    if !config.enabled {
        return AiCuration {
            picks: Vec::new(),
            warning: Some("AI curation is disabled.".to_string()),
        };
    }

    let base_url = config.base_url.trim();
    let model = config.model.trim();
    if base_url.is_empty() || model.is_empty() {
        return AiCuration {
            picks: Vec::new(),
            warning: Some("AI curation skipped: the API URL or model is empty.".to_string()),
        };
    }

    if count == 0 {
        return AiCuration::default();
    }
    if candidates.is_empty() {
        return AiCuration {
            picks: Vec::new(),
            warning: Some(
                "AI curation skipped: there are no candidates to choose from.".to_string(),
            ),
        };
    }

    let entries = prompt_candidates(candidates, MAX_PROMPT_CANDIDATES);
    let id_map: HashMap<String, usize> = entries
        .iter()
        .map(|(id, index)| (id.clone(), *index))
        .collect();
    let endpoint = chat_endpoint(base_url);
    let system = system_prompt(count);
    let user = user_prompt(profile, candidates, &entries, count);

    match request_curation(
        &endpoint,
        model,
        config.api_key.trim(),
        system,
        user,
        candidates,
        &id_map,
        count,
    )
    .await
    {
        Ok(curation) => curation,
        Err(error) => AiCuration {
            picks: Vec::new(),
            warning: Some(format!("AI curation failed: {error}")),
        },
    }
}

async fn request_curation(
    endpoint: &str,
    model: &str,
    api_key: &str,
    system: String,
    user: String,
    candidates: &[Candidate],
    id_map: &HashMap<String, usize>,
    count: usize,
) -> Result<AiCuration, String> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("creating the HTTP client: {error}"))?;

    let mut body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 0.6,
        "max_tokens": MAX_OUTPUT_TOKENS,
        "response_format": {"type": "json_object"},
    });

    let (status, text) = send_chat(&client, endpoint, api_key, &body).await?;
    if !status.is_success() {
        let retry_without_json_mode = (status == StatusCode::BAD_REQUEST
            || status == StatusCode::UNPROCESSABLE_ENTITY)
            && mentions_json_mode(&text);

        if retry_without_json_mode {
            if let Some(object) = body.as_object_mut() {
                object.remove("response_format");
            }
            let (retry_status, retry_text) = send_chat(&client, endpoint, api_key, &body).await?;
            if !retry_status.is_success() {
                return Err(format!(
                    "HTTP {}: {}",
                    retry_status.as_u16(),
                    body_snippet(&retry_text)
                ));
            }
            let response: Value = serde_json::from_str(&retry_text)
                .map_err(|error| format!("invalid JSON response: {error}"))?;
            return parse_chat_response(&response, candidates, id_map, count);
        }

        return Err(format!("HTTP {}: {}", status.as_u16(), body_snippet(&text)));
    }

    let response: Value =
        serde_json::from_str(&text).map_err(|error| format!("invalid JSON response: {error}"))?;
    parse_chat_response(&response, candidates, id_map, count)
}

async fn send_chat(
    client: &Client,
    endpoint: &str,
    api_key: &str,
    body: &Value,
) -> Result<(StatusCode, String), String> {
    let mut request = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(body);
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("network error: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("reading the response: {error}"))?;
    Ok((status, text))
}

fn mentions_json_mode(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("response_format")
        || lower.contains("json mode")
        || lower.contains("json_mode")
        || lower.contains("json_object")
}

fn body_snippet(body: &str) -> String {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet: String = normalized.chars().take(300).collect();
    if snippet.is_empty() {
        "empty response body".to_string()
    } else {
        snippet
    }
}

fn chat_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/chat/completions") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/chat/completions")
    }
}

fn parse_chat_response(
    response: &Value,
    candidates: &[Candidate],
    id_map: &HashMap<String, usize>,
    count: usize,
) -> Result<AiCuration, String> {
    let choice = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first());
    let content = choice
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(content_text);

    let Some(content) = content else {
        let finish_reason = choice
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let reasoning_tokens = response
            .get("usage")
            .and_then(|usage| usage.get("completion_tokens_details"))
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64);
        let detail = match reasoning_tokens {
            Some(tokens) if finish_reason == "length" => format!(
                "the model used its whole output budget on reasoning ({tokens} reasoning tokens) \
                 and returned no answer"
            ),
            Some(tokens) => format!(
                "the model returned no answer after {tokens} reasoning tokens (finish reason: {finish_reason})"
            ),
            None => format!(
                "the response contained no message content (finish reason: {finish_reason})"
            ),
        };
        return Err(detail);
    };

    parse_curation_content(&content, candidates, id_map, count)
}

fn content_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                _ => part.get("text").and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => return None,
    };

    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
}

fn parse_curation_content(
    content: &str,
    candidates: &[Candidate],
    id_map: &HashMap<String, usize>,
    count: usize,
) -> Result<AiCuration, String> {
    let json_text =
        extract_json(content).ok_or_else(|| "the response contained no JSON object".to_string())?;
    let (value, salvaged) = match serde_json::from_str::<Value>(json_text) {
        Ok(value) => (value, false),
        Err(error) => {
            // A long reasoning block can use up the output budget and cut the
            // answer off mid-array. Keep every pick object that arrived whole.
            let complete = salvage_pick_objects(content);
            if complete.is_empty() {
                return Err(format!("could not parse the AI JSON: {error}"));
            }
            (json!({ "picks": complete }), true)
        }
    };
    let picks = value
        .get("picks")
        .and_then(Value::as_array)
        .ok_or_else(|| "the AI JSON contained no picks array".to_string())?;

    let mut curated: Vec<(String, String)> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut artist_counts: HashMap<String, usize> = HashMap::new();

    for pick in picks {
        if curated.len() >= count {
            break;
        }

        let Some(id) = pick_id(pick) else {
            continue;
        };
        if !seen_ids.insert(id.clone()) {
            continue;
        }
        let Some(&index) = id_map.get(&id) else {
            continue;
        };
        let candidate = &candidates[index];

        let artist = candidate.artist.trim().to_lowercase();
        if artist_counts.get(&artist).copied().unwrap_or(0) >= 2 {
            continue;
        }

        let reason = pick
            .get("reason")
            .and_then(Value::as_str)
            .map(trim_reason)
            .unwrap_or_default();

        *artist_counts.entry(artist).or_default() += 1;
        curated.push((id, reason));
    }

    let warning = if salvaged {
        Some(format!(
            "The AI answer was cut off; used its {} complete pick{}. The rest were filled deterministically.",
            curated.len(),
            if curated.len() == 1 { "" } else { "s" }
        ))
    } else if curated.len() < count {
        Some(format!(
            "AI returned {} of {} valid picks",
            curated.len(),
            count
        ))
    } else {
        None
    };

    Ok(AiCuration {
        picks: curated,
        warning,
    })
}

/// Pull every complete `{...}` object out of a truncated `"picks": [...]`
/// array. Used when the model runs out of output tokens mid-answer.
fn salvage_pick_objects(text: &str) -> Vec<Value> {
    let Some(key) = text.find("\"picks\"") else {
        return Vec::new();
    };
    let rest = &text[key..];
    let Some(bracket) = rest.find('[') else {
        return Vec::new();
    };

    let bytes = rest.as_bytes();
    let mut objects = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    let mut in_string = false;
    let mut escaped = false;

    for (index, byte) in bytes.iter().enumerate().skip(bracket + 1) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            b'}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(start) = start.take() {
                        if let Ok(value) = serde_json::from_str::<Value>(&rest[start..=index]) {
                            objects.push(value);
                        }
                    }
                }
            }
            b']' if depth == 0 => break,
            _ => {}
        }
    }

    objects
}

fn pick_id(pick: &Value) -> Option<String> {
    match pick {
        Value::String(id) => non_empty(id),
        _ => pick.get("id").and_then(value_id),
    }
}

fn value_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => non_empty(id),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn trim_reason(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_REASON_CHARS {
        return normalized;
    }

    let mut trimmed: String = normalized.chars().take(MAX_REASON_CHARS).collect();
    if let Some(index) = trimmed.rfind(' ') {
        if index > 0 {
            trimmed.truncate(index);
        }
    }
    trimmed
}

fn prompt_candidates(candidates: &[Candidate], limit: usize) -> Vec<(String, usize)> {
    let mut used_ids: HashSet<String> = HashSet::new();
    let mut entries: Vec<(String, usize)> = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let base = {
            let id = candidate.id.trim();
            if id.is_empty() {
                format!("c{index:03}")
            } else {
                id.to_string()
            }
        };
        let mut id = base.clone();
        let mut suffix = 1;
        while !used_ids.insert(id.clone()) {
            suffix += 1;
            id = format!("{base}-{suffix}");
        }
        entries.push((id, index));
    }

    if entries.len() <= limit {
        return entries;
    }

    let mut ranked: Vec<usize> = (0..entries.len()).collect();
    ranked.sort_by(|&left, &right| {
        let left_score = prompt_score(&candidates[entries[left].1]);
        let right_score = prompt_score(&candidates[entries[right].1]);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.cmp(&right))
    });

    // First pass: keep a mix of sources. Second pass: fill any remaining
    // slots with the best candidates regardless of source.
    let per_source = (limit / 5).max(1);
    let mut per_source_counts = [0usize; 5];
    let mut taken = vec![false; entries.len()];
    let mut selected: Vec<(String, usize)> = Vec::with_capacity(limit);

    for &position in &ranked {
        if selected.len() >= limit {
            break;
        }
        let entry = &entries[position];
        let bucket = source_bucket(candidates[entry.1].source);
        if per_source_counts[bucket] >= per_source {
            continue;
        }
        per_source_counts[bucket] += 1;
        taken[position] = true;
        selected.push(entry.clone());
    }

    if selected.len() < limit {
        for &position in &ranked {
            if selected.len() >= limit {
                break;
            }
            if !taken[position] {
                selected.push(entries[position].clone());
            }
        }
    }

    selected.sort_by_key(|(_, index)| *index);
    selected
}

fn prompt_score(candidate: &Candidate) -> f64 {
    let popularity = ((1.0 + candidate.popularity as f64).ln() / 1001f64.ln()).clamp(0.0, 1.0);
    0.6 * candidate.source.weight() + 0.4 * popularity
}

fn source_bucket(source: crate::model::CandidateSource) -> usize {
    use crate::model::CandidateSource;
    match source {
        CandidateSource::RadioEasy => 0,
        CandidateSource::RadioMedium => 1,
        CandidateSource::RadioHard => 2,
        CandidateSource::Genre => 3,
        CandidateSource::Collaborative => 4,
    }
}

fn profile_summary(profile: &TasteProfile) -> String {
    let mut lines = Vec::new();

    let username = {
        let username = profile.username.trim();
        if username.is_empty() {
            "unknown"
        } else {
            username
        }
    };
    lines.push(format!(
        "Listener: {username} ({} listens)",
        profile.listen_count
    ));

    let artists = profile
        .artists
        .iter()
        .take(12)
        .map(|artist| {
            format!(
                "{} ({})",
                clean_field(&artist.name, 60),
                artist.listen_count
            )
        })
        .collect::<Vec<_>>();
    lines.push(format!("Top artists: {}", join_or_none(&artists)));

    let genres = profile
        .genres
        .iter()
        .take(10)
        .map(|genre| format!("{} ({})", clean_field(&genre.name, 40), genre.listen_count))
        .collect::<Vec<_>>();
    lines.push(format!("Top genres: {}", join_or_none(&genres)));

    let recordings = profile
        .recordings
        .iter()
        .take(8)
        .map(|recording| {
            format!(
                "\"{}\" — {} ({})",
                clean_field(&recording.title, 70),
                clean_field(&recording.artist, 60),
                recording.listen_count
            )
        })
        .collect::<Vec<_>>();
    lines.push(format!("Top recordings: {}", join_or_none(&recordings)));

    let recent = profile
        .recent
        .iter()
        .take(10)
        .map(|track| {
            format!(
                "\"{}\" — {}",
                clean_field(&track.title, 70),
                clean_field(&track.artist, 60)
            )
        })
        .collect::<Vec<_>>();
    lines.push(format!("Recent tracks: {}", join_or_none(&recent)));

    lines.push(format!("Era highlights: {}", era_highlights(profile)));

    lines.join("\n")
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

fn era_highlights(profile: &TasteProfile) -> String {
    if profile.eras.is_empty() {
        return "not available".to_string();
    }

    let mut decades: HashMap<i32, u64> = HashMap::new();
    for era in &profile.eras {
        *decades.entry(era.year.div_euclid(10) * 10).or_default() += era.listen_count;
    }

    let mut items: Vec<(i32, u64)> = decades.into_iter().collect();
    items.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    items
        .iter()
        .take(4)
        .map(|(decade, listens)| format!("{decade}s ({listens} listens)"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn user_prompt(
    profile: &TasteProfile,
    candidates: &[Candidate],
    entries: &[(String, usize)],
    count: usize,
) -> String {
    let mut text = String::from("Listener profile\n");
    text.push_str(&profile_summary(profile));
    text.push_str("\n\nCandidates (id|artist|title|album|year|source|seed|tags):\n");
    for (id, index) in entries {
        text.push_str(&candidate_line(id, &candidates[*index]));
        text.push('\n');
    }
    text.push_str(&format!(
        "\nChoose exactly {count} picks and return the JSON object now."
    ));
    text
}

fn candidate_line(id: &str, candidate: &Candidate) -> String {
    let album = candidate
        .album
        .as_deref()
        .map(|album| clean_field(album, 50))
        .filter(|album| !album.is_empty())
        .unwrap_or_else(|| "-".to_string());
    let year = candidate
        .year
        .map(|year| year.to_string())
        .unwrap_or_else(|| "?".to_string());
    let seed = candidate
        .seed_artist
        .as_deref()
        .map(|seed| clean_field(seed, 50))
        .filter(|seed| !seed.is_empty())
        .unwrap_or_else(|| "-".to_string());
    let tags = candidate
        .tags
        .iter()
        .map(|tag| clean_field(tag, 24))
        .filter(|tag| !tag.is_empty())
        .take(3)
        .collect::<Vec<_>>();
    let tags = if tags.is_empty() {
        "-".to_string()
    } else {
        tags.join(", ")
    };

    format!(
        "{id}|{}|{}|{album}|{year}|{}|{seed}|{tags}",
        clean_field(&candidate.artist, 60),
        clean_field(&candidate.title, 70),
        candidate.source.label(),
    )
}

fn clean_field(value: &str, limit: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized = normalized.replace('|', "/");
    normalized.chars().take(limit).collect()
}

fn system_prompt(count: usize) -> String {
    format!(
        "You are the curator for a personal music discovery app. Choose tracks from the supplied candidate pool only.\n\
         Rules:\n\
         - ONLY use ids from the candidate list. Never invent tracks, artists, albums, or ids.\n\
         - Return exactly {count} picks.\n\
         - Use at most 2 picks per artist.\n\
         - At least 40% of the picks must come from unfamiliar artists or exploratory sources \
           (Deeper cut, Listeners like you, Genre explorer).\n\
         - Mix eras instead of clustering in a single decade.\n\
         - Give each pick a short, concrete reason of at most 18 words, grounded in the listener profile. \
           Never invent facts.\n\
         - Reply with JSON only, with no markdown or commentary: \
           {{\"picks\":[{{\"id\":\"c001\",\"reason\":\"...\"}}]}}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, artist: &str) -> Candidate {
        Candidate {
            id: id.to_string(),
            mbid: format!("mbid-{id}"),
            artist: artist.to_string(),
            title: format!("Title {id}"),
            ..Candidate::default()
        }
    }

    fn id_map(candidates: &[Candidate]) -> HashMap<String, usize> {
        candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| (candidate.id.clone(), index))
            .collect()
    }

    #[test]
    fn extracts_json_from_prose_and_code_fences() {
        let text =
            "Here you go:\n```json\n{\"picks\":[{\"id\":\"c1\",\"reason\":\"ok\"}]}\n```\nDone.";
        assert_eq!(
            extract_json(text),
            Some("{\"picks\":[{\"id\":\"c1\",\"reason\":\"ok\"}]}")
        );
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn joins_content_parts() {
        let content = serde_json::json!([
            {"type": "text", "text": "{\"picks\":"},
            {"type": "text", "text": "[]}"}
        ]);
        assert_eq!(content_text(&content).as_deref(), Some("{\"picks\":[]}"));
        assert!(content_text(&Value::String("   ".to_string())).is_none());
    }

    #[test]
    fn salvages_complete_picks_from_a_truncated_answer() {
        let candidates = vec![
            candidate("c1", "Alpha"),
            candidate("c2", "Beta"),
            candidate("c3", "Gamma"),
        ];
        // The model ran out of tokens in the middle of the third pick.
        let content = r#"{"picks":[
            {"id":"c1","reason":"first"},
            {"id":"c2","reason":"second"},
            {"id":"c3","reason":"cut o"#;

        let curation =
            parse_curation_content(content, &candidates, &id_map(&candidates), 3).unwrap();
        assert_eq!(curation.picks.len(), 2);
        assert_eq!(curation.picks[0].0, "c1");
        assert_eq!(curation.picks[1].0, "c2");
        let warning = curation.warning.unwrap();
        assert!(warning.contains("cut off"), "{warning}");
    }

    #[test]
    fn extraction_only_counts_whole_objects() {
        let text = r#"{"picks":[{"id":"a"}, {"id":"b"}, {"id":"c""#;
        let objects = salvage_pick_objects(text);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[0]["id"], "a");
        assert_eq!(objects[1]["id"], "b");
    }

    #[test]
    fn normalizes_the_chat_endpoint() {
        assert_eq!(
            chat_endpoint("https://api.openai.com/v1/"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            chat_endpoint("https://example.test/v1/chat/completions/"),
            "https://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn ignores_unknown_ids_and_warns_when_short() {
        let candidates = vec![candidate("c1", "Alpha"), candidate("c2", "Beta")];
        let content = r#"{
            "picks": [
                {"id": "c1", "reason": "  A   good reason. "},
                {"id": "nope", "reason": "invented"},
                {"id": "c1", "reason": "duplicate"},
                {"id": "c2", "reason": "Another reason."}
            ]
        }"#;

        let curation =
            parse_curation_content(content, &candidates, &id_map(&candidates), 3).unwrap();
        assert_eq!(
            curation.picks,
            vec![
                ("c1".to_string(), "A good reason.".to_string()),
                ("c2".to_string(), "Another reason.".to_string()),
            ]
        );
        assert_eq!(
            curation.warning.as_deref(),
            Some("AI returned 2 of 3 valid picks")
        );
    }

    #[test]
    fn enforces_the_artist_cap_and_trims_reasons() {
        let candidates = vec![
            candidate("c1", "Repeat"),
            candidate("c2", "Repeat"),
            candidate("c3", "Repeat"),
            candidate("c4", "Other"),
        ];
        let long_reason = "word ".repeat(50);
        let content = format!(
            r#"{{"picks":[
                {{"id":"c1","reason":"one"}},
                {{"id":"c2","reason":"two"}},
                {{"id":"c3","reason":"three"}},
                {{"id":"c4","reason":"{long_reason}"}}
            ]}}"#
        );

        let curation =
            parse_curation_content(&content, &candidates, &id_map(&candidates), 4).unwrap();
        assert_eq!(curation.picks.len(), 3);
        assert!(curation.picks.iter().all(|(id, _)| id != "c3"));
        assert!(
            curation
                .picks
                .iter()
                .all(|(_, reason)| reason.chars().count() <= MAX_REASON_CHARS)
        );
        assert_eq!(
            curation.warning.as_deref(),
            Some("AI returned 3 of 4 valid picks")
        );
    }

    #[test]
    fn reads_content_from_choices_and_accepts_object_parts() {
        let candidates = vec![candidate("c1", "Alpha")];
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "text", "text": "{\"picks\":[{\"id\":\"c1\","},
                        {"type": "text", "text": "\"reason\":\"ok\"}]}"}
                    ]
                }
            }]
        });

        let curation =
            parse_chat_response(&response, &candidates, &id_map(&candidates), 1).unwrap();
        assert_eq!(curation.picks, vec![("c1".to_string(), "ok".to_string())]);
        assert!(curation.warning.is_none());
    }
}
