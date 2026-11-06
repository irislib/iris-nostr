//! Bind the public bootstrap to its advertised digest and revision-aware contents.
use iris_nostr::snapshot::{DEFAULT_IDENTITY, DEFAULT_SHA256};
use nostr::{EventBuilder, Keys, Kind};
use nostr_social_graph::SocialGraph;
use std::process::Command;
use tempfile::TempDir;

const BOOTSTRAP: &[u8] = include_bytes!("data/default-bootstrap-v4.bin");

#[test]
fn pinned_default_preserves_follow_and_mute_revisions() {
    assert_eq!(BOOTSTRAP.len(), 1_034_502);
    assert_eq!(BOOTSTRAP[0], 4);
    let graph =
        iris_nostr::snapshot::validate(BOOTSTRAP, Some(DEFAULT_SHA256), DEFAULT_IDENTITY).unwrap();
    assert_eq!(graph.size().users, 21_323);
    assert_eq!(graph.get_followed_by_user(graph.get_root()).len(), 2);
    let mut revisions = graph.get_list_revisions();
    let count = |kind: u32, complete: bool| {
        revisions
            .iter()
            .filter(|(_, actual, revision)| *actual == kind && (!complete || revision.complete))
            .count()
    };
    assert_eq!((count(3, false), count(3, true)), (283, 166));
    assert_eq!((count(10_000, false), count(10_000, true)), (101, 100));
    assert!(revisions.iter().all(|(_, _, revision)| {
        revision.id.len() == 64 && revision.id.bytes().all(|byte| byte.is_ascii_hexdigit())
    }));

    // A bootstrap projection remains partial. Reopening must retain that fact
    // along with each follow/mute event ID rather than inventing complete lists.
    let restored = SocialGraph::from_binary(graph.get_root(), &graph.to_binary().unwrap()).unwrap();
    let mut restored_revisions = restored.get_list_revisions();
    revisions.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    restored_revisions.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    assert_eq!(restored_revisions, revisions);
}

#[test]
fn an_existing_personal_cache_is_used_without_replacing_it_with_the_default() {
    let dir = TempDir::new().unwrap();
    let key = Keys::generate();
    let identity = key.public_key().to_hex();
    let graph = SocialGraph::new(&identity);
    let personal = graph.to_binary().unwrap();
    let cache = dir.path().join("iris-nostr/social-graph.bin");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, &personal).unwrap();
    let event = EventBuilder::new(Kind::TextNote, "synthetic local cached note")
        .sign_with_keys(&key)
        .unwrap();
    let events = dir.path().join("events.json");
    std::fs::write(&events, serde_json::to_vec(&[&event]).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iris-nostr"))
        .env_remove("IRIS_NOSTR_CONFIG")
        .env("XDG_CACHE_HOME", dir.path())
        .args([
            "events",
            "query",
            "--events",
            events.to_str().unwrap(),
            "--identity",
            &identity,
            "--offline",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["events"][0]["event"]["id"], event.id.to_hex());
    assert_eq!(std::fs::read(cache).unwrap(), personal);
}
