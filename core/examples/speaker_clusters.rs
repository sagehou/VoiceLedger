use std::io;

use serde::Deserialize;
use voiceledger_core::speakers::SpeakerClusters;

#[derive(Deserialize)]
struct Input {
    threshold: f64,
    embeddings: Vec<Option<Vec<f32>>>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input: Input = serde_json::from_reader(io::stdin().lock())?;
    let mut clusters =
        SpeakerClusters::new(input.threshold).ok_or("Invalid clustering threshold")?;
    let output: Vec<_> = input
        .embeddings
        .into_iter()
        .map(|embedding| embedding.and_then(|values| clusters.observe(&values)))
        .collect();
    serde_json::to_writer(io::stdout().lock(), &output)?;
    Ok(())
}
