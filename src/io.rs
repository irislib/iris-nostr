use anyhow::{Context, Result, ensure};
use nostr::Event;
use std::{fs::File, io::Read, path::Path};

pub const MAX_INPUT: u64 = 32 * 1024 * 1024;

pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .context("open input")?
        .take(MAX_INPUT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_INPUT, "input exceeds 32 MiB");
    Ok(bytes)
}

/// Signed Nostr JSON array, single event, or JSONL. Verify before graph ingestion.
pub fn read_events(path: &Path) -> Result<Vec<Event>> {
    let bytes = read_bounded(path)?;
    let text = std::str::from_utf8(&bytes)?.trim();
    let events: Vec<Event> = if text.starts_with('[') {
        serde_json::from_str(text)?
    } else if let Ok(event) = serde_json::from_str::<Event>(text) {
        vec![event]
    } else {
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?
    };
    for event in &events {
        event.verify().context("invalid event ID or signature")?;
    }
    Ok(events)
}
