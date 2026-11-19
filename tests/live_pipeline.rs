//! End-to-end check against the live ListenBrainz API.
//!
//! Run it with a public ListenBrainz username and no AI key:
//!
//!     cargo test --test live_pipeline -- --ignored --nocapture
//!
//! The test is ignored by default so `cargo test` never needs the network.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use lbreco::model::{Config, DiscoveryConfig, DiscoveryMode, ListenBrainzConfig, Progress};
use lbreco::pipeline;

#[tokio::test]
#[ignore = "hits the network and is rate limited to about one request per second"]
async fn generates_picks_for_a_real_profile() {
    let username = match std::env::var("LBRECO_LIVE_USER") {
        Ok(username) => username,
        Err(_) => {
            eprintln!("set LBRECO_LIVE_USER=<listenbrainz-username> to run this test");
            return;
        }
    };

    let config = Config {
        listenbrainz: ListenBrainzConfig {
            username,
            token: std::env::var("LISTENBRAINZ_TOKEN").unwrap_or_default(),
        },
        ai: Default::default(),
        discovery: DiscoveryConfig {
            count: 50,
            mode: DiscoveryMode::Mixed,
            include_genres: true,
            skip_recommended: false,
            avoid_repeats_days: 0,
        },
    };

    let (tx, rx) = async_channel::bounded(128);
    let cancel = Arc::new(AtomicBool::new(false));

    let collector = tokio::spawn(async move {
        let mut result = None;
        let mut stages = 0;
        while let Ok(message) = rx.recv().await {
            match message {
                Progress::Stage { fraction, text } => {
                    stages += 1;
                    eprintln!("[{:>3}%] {text}", (fraction * 100.0) as u32);
                }
                Progress::Done(run) => result = Some(*run),
                other => panic!("unexpected progress message: {other:?}"),
            }
        }
        eprintln!("{stages} progress updates");
        result
    });

    tokio::time::timeout(
        Duration::from_secs(600),
        pipeline::generate(config, tx, cancel),
    )
    .await
    .expect("the pipeline should finish within ten minutes")
    .expect("the pipeline should succeed");

    let result = collector
        .await
        .expect("the collector should finish")
        .expect("the pipeline should send a Done message");

    assert_eq!(result.recommendations.len(), 50);
    for warning in &result.summary.warnings {
        eprintln!("warning: {warning}");
    }
    for (index, recommendation) in result.recommendations.iter().enumerate() {
        assert_eq!(recommendation.rank, index + 1);
    }
    assert!(
        result
            .recommendations
            .iter()
            .all(|r| !r.reason.trim().is_empty())
    );
    assert!(
        result
            .recommendations
            .iter()
            .all(|r| r.musicbrainz_url.contains(&r.mbid))
    );
    assert!(
        result
            .recommendations
            .iter()
            .all(|r| r.youtube_url.starts_with("https://")),
    );

    // How diverse is the result? The caps widen only when the pool is too
    // narrow, so this should still show a spread of artists.
    let mut per_artist: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for recommendation in &result.recommendations {
        *per_artist
            .entry(recommendation.artist.as_str())
            .or_default() += 1;
    }
    let mut counts: Vec<usize> = per_artist.values().copied().collect();
    counts.sort_unstable_by(|a, b| b.cmp(a));
    eprintln!(
        "distinct artists: {}; most tracks by one artist: {}",
        counts.len(),
        counts.first().copied().unwrap_or(0)
    );

    for rec in result.recommendations.iter().take(5) {
        eprintln!(
            "{}. {} — {} ({})",
            rec.rank, rec.artist, rec.title, rec.reason
        );
    }
}
