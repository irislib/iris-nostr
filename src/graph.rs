use anyhow::{Context, Result};
use nostr::PublicKey;
use nostr_social_graph::{NostrEvent, SocialGraph};
use std::path::Path;

pub fn public_key(value: &str) -> Result<PublicKey> {
    crate::reference::public_key(value)
}

pub fn load_graph(identity: &str, path: &Path) -> Result<SocialGraph> {
    load_bytes(identity, path, &crate::io::read_bounded(path)?)
}

fn load_bytes(identity: &str, path: &Path, bytes: &[u8]) -> Result<SocialGraph> {
    let root = public_key(identity)?.to_hex();
    if path.extension().is_some_and(|ext| ext == "bin") {
        let mut graph =
            SocialGraph::from_binary(&root, bytes).context("read social graph snapshot")?;
        graph.recalculate_follow_distances();
        return Ok(graph);
    }
    let mut graph = SocialGraph::new(&root);
    let mut events = crate::io::read_events_from(bytes)?;
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

pub fn status(identity: &str, path: &Path) -> Result<serde_json::Value> {
    let bytes = crate::io::read_bounded(path)?;
    let digest = hashtree_core::to_hex(&hashtree_core::sha256(&bytes));
    let graph = load_bytes(identity, path, &bytes)?;
    let stats = graph.size();
    let modified = std::fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    Ok(
        serde_json::json!({"path":path,"identity":graph.get_root(),"sha256":digest,"bytes":bytes.len(),
        "direct_follows":graph.get_followed_by_user(graph.get_root()).len(),
        "is_pinned_bootstrap":digest==crate::snapshot::DEFAULT_SHA256,"local_file_modified_at":modified,
        "root_follow_list_created_at":graph.get_follow_list_created_at(graph.get_root()),
        "root_mute_list_created_at":graph.get_mute_list_created_at(graph.get_root()),
        "root_follow_list_known":graph.get_follow_list_created_at(graph.get_root()).is_some(),
        "users":stats.users,"follows":stats.follows,"mutes":stats.mutes,
        "authors_within_two_hops":graph.users_in_distance_order(Some(2)).len(),
        "authors_within_three_hops":graph.users_in_distance_order(Some(3)).len(),
        "coverage":"observed lists; file time and event timestamps do not prove freshness or complete coverage"}),
    )
}
