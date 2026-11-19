use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use async_channel::Sender;

use crate::ai;
use crate::config::History;
use crate::curate;
use crate::lb::LbClient;
use crate::model::{Config, Progress, RunResult, RunSummary};
use crate::radio;

pub async fn generate(
    config: Config,
    progress: Sender<Progress>,
    cancel: Arc<AtomicBool>,
) -> Result<()> {
    let result = generate_inner(&config, &progress, &cancel).await;
    match result {
        Ok(run) => {
            let _ = progress
                .send(Progress::Stage {
                    fraction: 1.0,
                    text: "Done".to_string(),
                })
                .await;
            let _ = progress.send(Progress::Done(Box::new(run))).await;
            Ok(())
        }
        Err(error) => {
            if cancel.load(Ordering::Relaxed) {
                let _ = progress.send(Progress::Cancelled).await;
            } else {
                let _ = progress.send(Progress::Failed(error.to_string())).await;
            }
            Err(error)
        }
    }
}

async fn generate_inner(
    config: &Config,
    progress: &Sender<Progress>,
    cancel: &AtomicBool,
) -> Result<RunResult> {
    config.validate().map_err(anyhow::Error::msg)?;
    check_cancel(cancel)?;

    let stage = |fraction: f64, text: &str| {
        let _ = progress.try_send(Progress::Stage {
            fraction,
            text: text.to_string(),
        });
    };

    stage(0.02, "Checking your ListenBrainz profile");
    let client = LbClient::new(
        config.listenbrainz.username.clone(),
        config.listenbrainz.token.clone(),
    )?;

    stage(0.06, "Reading your listening history");
    let profile = client
        .taste_profile()
        .await
        .context("Could not read your ListenBrainz taste profile")?;
    check_cancel(cancel)?;
    stage(0.30, "Found your taste profile");

    let mut warnings = Vec::new();
    if profile.artists.is_empty() {
        warnings.push(
            "ListenBrainz artist stats are not available yet; using recommendations and genres instead."
                .to_string(),
        );
    }
    if profile.genres.is_empty() {
        warnings.push("No genre stats yet; genre exploration is limited.".to_string());
    }

    let history = History::load();
    let history_mbids = if config.discovery.skip_recommended {
        history.recent_mbids(config.discovery.avoid_repeats_days)
    } else {
        Default::default()
    };

    stage(0.32, "Looking for artists in your lane");
    let mut radio_progress = |fraction: f64, text: String| {
        let overall = 0.32 + fraction * 0.34;
        let _ = progress.try_send(Progress::Stage {
            fraction: overall,
            text,
        });
    };
    let pool = radio::build_candidates(
        &client,
        &profile,
        &config.discovery,
        &history_mbids,
        &mut radio_progress,
    )
    .await
    .context("Could not build a candidate pool")?;
    warnings.extend(pool.warnings);
    check_cancel(cancel)?;

    if pool.candidates.len() < config.discovery.count {
        warnings.push(format!(
            "Only {} unique candidates were available for a {}-track run.",
            pool.candidates.len(),
            config.discovery.count
        ));
    }

    let mut ai_warning = None;
    let mut ai_picks = Vec::new();
    if config.ai_ready() {
        stage(
            0.70,
            &format!("Asking {} to curate the shortlist", config.ai.model),
        );
        let curated = ai::curate(
            &profile,
            &pool.candidates,
            &config.ai,
            config.discovery.count,
        )
        .await;
        ai_picks = curated.picks;
        ai_warning = curated.warning;
    } else if config.ai.enabled {
        ai_warning =
            Some("AI is enabled but incomplete; using deterministic curation.".to_string());
    }
    if let Some(warning) = ai_warning {
        warnings.push(warning);
    }
    check_cancel(cancel)?;

    stage(0.90, "Balancing artists, eras and reasons");
    let recommendations = curate::build_recommendations(
        &profile,
        &pool.candidates,
        &ai_picks,
        config.discovery.count,
        config.discovery.mode,
    );

    if recommendations.is_empty() {
        bail!(
            "No recommendations were produced. Try a different ListenBrainz profile or discovery mode."
        );
    }
    if recommendations.len() < config.discovery.count {
        warnings.push(format!(
            "Showing {} picks because the candidate pool ran out.",
            recommendations.len()
        ));
    }

    let ai_used = !ai_picks.is_empty() && config.ai_ready();
    let summary = RunSummary {
        username: profile.username.clone(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        listen_count: profile.listen_count,
        top_artists: profile.top_artist_names(),
        top_genres: profile.top_genre_names(),
        ai_used,
        ai_model: ai_used.then(|| config.ai.model.clone()),
        warnings,
    };

    let result = RunResult {
        summary,
        recommendations,
    };

    stage(0.97, "Remembering these picks");
    let mut history = history;
    history.remember(
        result
            .recommendations
            .iter()
            .map(|recommendation| recommendation.mbid.clone()),
    );
    if let Err(error) = history.save() {
        let mut result = result;
        result
            .summary
            .warnings
            .push(format!("Could not save recommendation history: {error}"));
        return Ok(result);
    }

    Ok(result)
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }
    Ok(())
}
