//! Legacy imported snapshots can assign several numeric IDs to one public key.
//! Exercise the CLI's actual load/filter path, not only the dependency's parser.
use iris_nostr::{Request, Sources, query};
use nostr::{EventBuilder, Filter, Keys, Kind, Timestamp};
use std::{process::Command, time::Duration};
use tempfile::TempDir;

type Lists = Vec<(u8, u8, Vec<u8>)>;

fn aliased_snapshot(ids: &[(u8, String)], follows: Lists, mutes: Lists) -> Vec<u8> {
    // Version 3, string-form node IDs. All fixture counts/IDs/times fit one varint byte.
    let mut bytes = vec![3, ids.len().try_into().unwrap()];
    for (id, key) in ids {
        assert_eq!(key.len(), 64);
        bytes.extend([*id, 2, 64]);
        bytes.extend(key.as_bytes());
    }
    for lists in [follows, mutes] {
        bytes.push(lists.len().try_into().unwrap());
        for (owner, at, targets) in lists {
            bytes.extend([owner, at, targets.len().try_into().unwrap()]);
            bytes.extend(targets);
        }
    }
    bytes
}

#[tokio::test]
async fn imported_aliases_preserve_offline_filter_mutes_and_native_status() {
    let dir = TempDir::new().unwrap();
    let root = Keys::generate();
    let friend = Keys::generate();
    let muted = Keys::generate();
    let stranger = Keys::generate();
    let root_hex = root.public_key().to_hex();
    let friend_hex = friend.public_key().to_hex();
    let muted_hex = muted.public_key().to_hex();
    let ids = [
        (0, root_hex.clone()),
        (1, friend_hex.clone()),
        (2, muted_hex.clone()),
        (7, root_hex.to_uppercase()),
        (8, friend_hex.to_uppercase()),
        (9, muted_hex.to_uppercase()),
    ];
    let bytes = aliased_snapshot(
        &ids,
        vec![(0, 10, vec![8, 9]), (7, 20, vec![1, 2])],
        vec![(0, 10, vec![2]), (7, 11, vec![9])],
    );
    let path = dir.path().join("personal.bin");
    std::fs::write(&path, &bytes).unwrap();
    let graph = iris_nostr::graph::load_graph(&root_hex, &path).unwrap();
    assert_eq!(graph.size().users, 3);
    assert_eq!(graph.get_followed_by_user(&root_hex).len(), 2);
    assert_eq!(graph.get_follow_distance(&friend_hex), 1);
    assert_eq!(graph.get_muted_by_user(&root_hex).len(), 1);
    assert!(graph.get_muted_by_user(&root_hex).contains(&muted_hex));
    assert_eq!(graph.get_follow_list_created_at(&root_hex), Some(20));
    assert_eq!(graph.get_mute_list_created_at(&root_hex), Some(11));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    let notes: Vec<_> = [&friend, &muted, &stranger]
        .into_iter()
        .enumerate()
        .map(|(i, key)| {
            EventBuilder::new(Kind::TextNote, "local synthetic note")
                .custom_created_at(Timestamp::from(30 + i as u64))
                .sign_with_keys(key)
                .unwrap()
        })
        .collect();
    let events = dir.path().join("notes.json");
    std::fs::write(&events, serde_json::to_vec(&notes).unwrap()).unwrap();
    let result = query(Request {
        sources: Sources {
            events: vec![events],
            graph: Some(path.clone()),
            identity: Some(root_hex.clone()),
            max_distance: Some(1),
            ..Default::default()
        },
        filter: Filter::new().kind(Kind::TextNote),
        limit: 50,
        fetch_limit: 100,
        timeout: Duration::from_secs(1),
        offline: true,
        contains: Vec::new(),
    })
    .await
    .unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["events"][0]["event"]["id"], notes[0].id.to_hex());
    let output = Command::new(env!("CARGO_BIN_EXE_iris-nostr"))
        .args([
            "graph",
            "status",
            "--graph",
            path.to_str().unwrap(),
            "--identity",
            &root_hex,
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["users"], 3);
    assert_eq!(status["direct_follows"], 2);
    assert_eq!(status["mutes"], 1);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn conflicting_imports_fail_without_replacing_personal_snapshot() {
    let dir = TempDir::new().unwrap();
    let root = Keys::generate().public_key().to_hex();
    let a = Keys::generate().public_key().to_hex();
    let b = Keys::generate().public_key().to_hex();
    let ids = [(0, root.clone()), (1, a), (2, b), (7, root.to_uppercase())];
    let path = dir.path().join("personal.bin");
    let bytes = aliased_snapshot(&ids, vec![(0, 10, vec![1]), (7, 10, vec![2])], vec![]);
    std::fs::write(&path, &bytes).unwrap();
    assert!(iris_nostr::graph::load_graph(&root, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);

    // Unknown equal-time mute bodies are retained conservatively, never lost.
    let bytes = aliased_snapshot(
        &ids,
        vec![(0, 10, vec![1, 2])],
        vec![(0, 10, vec![1]), (7, 10, vec![2])],
    );
    std::fs::write(&path, &bytes).unwrap();
    let graph = iris_nostr::graph::load_graph(&root, &path).unwrap();
    assert_eq!(graph.get_muted_by_user(&root).len(), 2);
    assert!(graph.get_list_revision(&root, 10_000).is_none());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
