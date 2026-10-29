use anyhow::{Context, Result};
use nostr::PublicKey;
use nostr_social_graph::{NostrEvent, SocialGraph};
use std::path::Path;

pub fn public_key(value: &str) -> Result<PublicKey> {
    crate::reference::public_key(value)
}

pub fn load_graph(identity: &str, path: &Path) -> Result<SocialGraph> {
    let root = public_key(identity)?.to_hex();
    if path.extension().is_some_and(|ext| ext == "bin") {
        let mut graph = SocialGraph::from_binary(&root, &crate::io::read_bounded(path)?)
            .context("read social graph snapshot")?;
        graph.recalculate_follow_distances();
        return Ok(graph);
    }
    let mut graph = SocialGraph::new(&root);
    let mut events = crate::io::read_events(path)?;
    events.sort_by_key(|event| (event.created_at, event.id));
    for event in events {
        graph.handle_event(
            &NostrEvent {
                created_at: event.created_at.as_secs(),
                content: event.content,
                tags: event
                    .tags
                    .iter()
                    .map(|tag| tag.as_slice().to_vec())
                    .collect(),
                kind: event.kind.as_u16().into(),
                pubkey: event.pubkey.to_hex(),
                id: event.id.to_hex(),
                sig: event.sig.to_string(),
            },
            true,
            1.0,
        );
    }
    graph.recalculate_follow_distances();
    Ok(graph)
}
