use iris_nostr::{Request, Sources, query};
use nostr::{Event, EventBuilder, Filter, Keys, Kind, Tag, Timestamp};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tempfile::TempDir;

fn note(keys: &Keys, time: u64, text: &str) -> Event {
    EventBuilder::new(Kind::TextNote, text)
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(keys)
        .unwrap()
}
fn save(dir: &TempDir, name: &str, events: &[Event]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, serde_json::to_vec(events).unwrap()).unwrap();
    path
}
fn request(mut sources: Sources) -> Request {
    sources.no_social_filter = sources.graph.is_none();
    Request {
        sources,
        filter: Filter::new().kind(Kind::TextNote),
        limit: 50,
        fetch_limit: 1000,
        timeout: Duration::from_secs(2),
        offline: true,
        contains: Vec::new(),
    }
}

#[tokio::test]
async fn local_query_deduplicates_and_retains_provenance() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::generate();
    let old = note(&keys, 1, "old");
    let new = note(&keys, 2, "new");
    let a = save(&dir, "a.json", &[old.clone(), new.clone()]);
    let b = save(&dir, "b.json", std::slice::from_ref(&new));
    let report = query(request(Sources {
        events: vec![a, b],
        ..Default::default()
    }))
    .await
    .unwrap();
    assert_eq!(report["partial"], false);
    assert_eq!(report["events"].as_array().unwrap().len(), 2);
    assert_eq!(report["events"][0]["event"]["id"], new.id.to_hex());
    assert_eq!(report["events"][0]["sources"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn graph_filter_precedes_output_limit_and_respects_mutes() {
    let dir = TempDir::new().unwrap();
    let root = Keys::generate();
    let friend = Keys::generate();
    let muted = Keys::generate();
    let stranger = Keys::generate();
    let follows = EventBuilder::new(Kind::ContactList, "")
        .tags([
            Tag::public_key(friend.public_key()),
            Tag::public_key(muted.public_key()),
        ])
        .sign_with_keys(&root)
        .unwrap();
    let mutes = EventBuilder::new(Kind::from(10000), "")
        .tags([Tag::public_key(muted.public_key())])
        .sign_with_keys(&root)
        .unwrap();
    let graph = save(&dir, "graph.json", &[follows, mutes]);
    let accepted = note(&friend, 1, "friend");
    let events = save(
        &dir,
        "events.json",
        &[
            note(&stranger, 3, "stranger"),
            note(&muted, 2, "muted"),
            accepted.clone(),
        ],
    );
    let mut req = request(Sources {
        events: vec![events],
        graph: Some(graph),
        identity: Some(root.public_key().to_hex()),
        max_distance: Some(1),
        ..Default::default()
    });
    req.limit = 1;
    let result = query(req).await.unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["events"][0]["event"]["id"], accepted.id.to_hex());
}

#[tokio::test]
async fn rejects_forged_events_and_missing_graph_policy_input() {
    let dir = TempDir::new().unwrap();
    let keys = Keys::generate();
    let mut event = note(&keys, 1, "valid");
    event.content = "forged".into();
    let file = save(&dir, "forged.json", &[event]);
    assert!(
        query(request(Sources {
            events: vec![file],
            ..Default::default()
        }))
        .await
        .is_err()
    );
    assert!(
        query(request(Sources {
            events: vec![PathBuf::from("absent")],
            max_distance: Some(1),
            ..Default::default()
        }))
        .await
        .unwrap_err()
        .to_string()
        .contains("open input")
    );
}

#[tokio::test]
async fn offline_rejects_daemon_and_relays_before_reading_inputs() {
    for sources in [
        Sources {
            daemon: Some("http://127.0.0.1:8080".into()),
            ..Default::default()
        },
        Sources {
            relays: vec!["wss://example.invalid".into()],
            ..Default::default()
        },
    ] {
        assert!(
            query(request(sources))
                .await
                .unwrap_err()
                .to_string()
                .contains("offline")
        );
    }
}

#[tokio::test]
async fn hashtree_index_and_missing_partition_report_partial() {
    use hashtree_core::{MemoryStore, Store, to_hex};
    use hashtree_nostr_pubsub::HashtreeNostrBoundedEventCache;
    use nostr_pubsub::{EventBus, EventRetentionPolicy, EventSource, VerifiedEvent};
    let dir = TempDir::new().unwrap();
    let keys = Keys::generate();
    let event = note(&keys, 1, "indexed");
    let store = Arc::new(MemoryStore::new());
    let cache = HashtreeNostrBoundedEventCache::new(
        store.clone(),
        None,
        EventSource::local_index("fixture"),
        EventRetentionPolicy::new(10, vec![Filter::new()]),
    );
    cache
        .publish(
            VerifiedEvent::try_from(event.clone()).unwrap(),
            EventSource::local_index("fixture"),
        )
        .await
        .unwrap();
    for hash in store.keys() {
        std::fs::write(
            dir.path().join(to_hex(&hash)),
            store.get(&hash).await.unwrap().unwrap(),
        )
        .unwrap();
    }
    let root = cache.root_cid().await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let daemon_url = format!("http://{}", listener.local_addr().unwrap());
    let shared_store = store.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = vec![0; 8192];
            let size = stream.read(&mut headers).await.unwrap();
            let headers = String::from_utf8_lossy(&headers[..size]);
            let path = headers
                .split_whitespace()
                .nth(1)
                .unwrap()
                .trim_start_matches('/');
            let hash = hashtree_core::from_hex(path).unwrap();
            let body = shared_store.get(&hash).await.unwrap().unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        }
    });
    let mut daemon_request = request(Sources {
        indexes: vec![root.to_string()],
        daemon: Some(daemon_url),
        ..Default::default()
    });
    daemon_request.offline = false;
    let daemon_report = query(daemon_request).await.unwrap();
    server.abort();
    assert_eq!(daemon_report["partial"], false);
    assert_eq!(daemon_report["events"][0]["event"]["id"], event.id.to_hex());

    let sources = Sources {
        indexes: vec![root.to_string(), "ab".repeat(32)],
        blocks: Some(dir.path().into()),
        ..Default::default()
    };
    let report = query(request(sources)).await.unwrap();
    assert_eq!(report["partial"], true);
    assert_eq!(report["events"][0]["event"]["id"], event.id.to_hex());
    assert_eq!(
        report["sources"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["ok"] == false)
            .count(),
        1
    );
}

#[tokio::test]
async fn block_source_verifies_hash_and_cannot_write() {
    use hashtree_core::Store;
    let dir = TempDir::new().unwrap();
    let hash = [1; 32];
    std::fs::write(dir.path().join("01".repeat(32)), b"wrong").unwrap();
    let store =
        iris_nostr::blocks::Blocks::new(Some(dir.path().into()), None, Duration::from_secs(1))
            .unwrap();
    assert!(
        store
            .get(&hash)
            .await
            .unwrap_err()
            .to_string()
            .contains("hash mismatch")
    );
    assert!(store.put(hash, vec![]).await.is_err());
    assert!(store.delete(&hash).await.is_err());
}

#[test]
fn daemon_boundary_is_loopback_without_redirects_or_credentials() {
    use iris_nostr::blocks::daemon_url;
    assert!(daemon_url("http://127.0.0.1:8080").is_ok());
    assert!(daemon_url("http://[::1]:8080").is_ok());
    for value in [
        "http://example.com",
        "http://user@127.0.0.1",
        "https://127.0.0.1",
        "http://127.0.0.1/path",
        "http://localhost",
    ] {
        assert!(daemon_url(value).is_err());
    }
}

#[tokio::test]
async fn relay_query_waits_for_eose_and_filters_irrelevant_events() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let keys = Keys::generate();
    let event = note(&keys, 1, "relay");
    let expected = event.id;
    let irrelevant = EventBuilder::new(Kind::Metadata, "{}")
        .sign_with_keys(&keys)
        .unwrap();
    let relay = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = socket.next().await {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&message.into_data())
            else {
                continue;
            };
            if value[0] == "REQ" {
                socket
                    .send(Message::Text(
                        serde_json::json!(["EVENT", value[1], irrelevant])
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(Message::Text(
                        serde_json::json!(["EVENT", value[1], event])
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(Message::Text(
                        serde_json::json!(["EOSE", value[1]]).to_string().into(),
                    ))
                    .await
                    .unwrap();
            }
        }
    });
    let mut req = request(Sources {
        relays: vec![url.clone()],
        ..Default::default()
    });
    req.offline = false;
    let report = query(req).await.unwrap();
    assert_eq!(report["partial"], false);
    assert_eq!(report["sources"].as_array().unwrap().len(), 1);
    assert_eq!(report["sources"][0]["relay"], url);
    assert_eq!(report["events"][0]["event"]["id"], expected.to_hex());
    relay.abort();
}

#[tokio::test]
async fn default_three_hops_includes_third_but_not_fourth_and_honors_override() {
    let dir = TempDir::new().unwrap();
    let keys: Vec<_> = (0..5).map(|_| Keys::generate()).collect();
    let root = keys[0].public_key().to_hex();
    let mut graph = nostr_social_graph::SocialGraph::new(&root);
    for pair in keys.windows(2) {
        graph
            .add_positive_relation(
                &pair[0].public_key().to_hex(),
                &pair[1].public_key().to_hex(),
                1,
            )
            .unwrap();
    }
    let graph_path = dir.path().join("graph.bin");
    std::fs::write(&graph_path, graph.to_binary().unwrap()).unwrap();
    let third = note(&keys[3], 3, "third");
    let events = save(
        &dir,
        "events.json",
        &[third.clone(), note(&keys[4], 4, "fourth")],
    );
    let sources = Sources {
        events: vec![events],
        identity: Some(root),
        graph: Some(graph_path),
        ..Default::default()
    };
    let report = query(request(sources.clone())).await.unwrap();
    assert_eq!(report["events"].as_array().unwrap().len(), 1);
    assert_eq!(report["events"][0]["event"]["id"], third.id.to_hex());
    assert_eq!(report["events"][0]["follow_distance"], 3);
    let report = query(request(Sources {
        max_distance: Some(2),
        ..sources
    }))
    .await
    .unwrap();
    assert!(report["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn large_graph_uses_bounded_local_policy_without_oversized_relay_filters() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    let dir = TempDir::new().unwrap();
    let owner = Keys::generate();
    let friend = Keys::generate();
    let muted = Keys::generate();
    let root = owner.public_key().to_hex();
    let mut graph = nostr_social_graph::SocialGraph::new(&root);
    for i in 1..=20_001 {
        graph
            .add_positive_relation(&root, &format!("{i:064x}"), 1)
            .unwrap();
    }
    graph
        .add_positive_relation(&root, &friend.public_key().to_hex(), 1)
        .unwrap();
    graph
        .add_positive_relation(&root, &muted.public_key().to_hex(), 1)
        .unwrap();
    graph
        .add_negative_relation(&root, &muted.public_key().to_hex(), 1)
        .unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, graph.to_binary().unwrap()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let accepted = note(&friend, 1, "beef");
    let expected = accepted.id.to_hex();
    let relay = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let value = loop {
            let message = ws.next().await.unwrap().unwrap();
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&message.into_data())
                && value[0] == "REQ"
            {
                break value;
            }
        };
        assert_eq!(value[0], "REQ");
        assert!(value[2].get("authors").is_none());
        assert_eq!(value[2]["kinds"], serde_json::json!([1]));
        assert_eq!(value[2]["limit"], 1000);
        for event in [
            note(&Keys::generate(), 4, "stranger"),
            note(&muted, 3, "muted"),
            note(&friend, 2, "unrelated"),
            accepted,
        ] {
            ws.send(Message::Text(
                serde_json::json!(["EVENT", value[1], event])
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        }
        ws.send(Message::Text(
            serde_json::json!(["EOSE", value[1]]).to_string().into(),
        ))
        .await
        .unwrap();
        while ws.next().await.is_some() {}
    });
    let mut req = request(Sources {
        relays: vec![url],
        identity: Some(root),
        graph: Some(path),
        ..Default::default()
    });
    req.offline = false;
    req.limit = 1;
    req.contains = vec!["BEEF".into()];
    let report = query(req).await.unwrap();
    assert_eq!(report["partial"], false);
    assert_eq!(report["graph_filter_location"], "local");
    assert!(report["graph_author_count"].as_u64().unwrap() > 20_000);
    assert_eq!(report["events"].as_array().unwrap().len(), 1);
    assert_eq!(report["events"][0]["event"]["id"], expected);
    assert_eq!(report["sources"][0]["counts"]["graph_rejected"], 2);
    assert_eq!(report["sources"][0]["counts"]["text_rejected"], 1);
    relay.abort();
}

#[tokio::test]
async fn unavailable_relay_is_partial_not_empty_success() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    drop(listener);
    let mut req = request(Sources {
        relays: vec![url],
        ..Default::default()
    });
    req.offline = false;
    let report = query(req).await.unwrap();
    assert_eq!(report["partial"], true);
    assert_eq!(report["sources"][0]["ok"], false);
}

#[test]
fn cli_help_and_jsonl_offline_roundtrip() {
    let binary = env!("CARGO_BIN_EXE_iris-nostr");
    let help = std::process::Command::new(binary)
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Iris Chat"));
    let dir = TempDir::new().unwrap();
    let event = note(&Keys::generate(), 1, "roundtrip");
    let input = save(&dir, "input.json", std::slice::from_ref(&event));
    let output = std::process::Command::new(binary)
        .args([
            "events",
            "query",
            "--offline",
            "--no-social-filter",
            "--jsonl",
            "--events",
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decoded: Event = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decoded, event);
    let cache = dir.path().join("cached.jsonl");
    std::fs::write(&cache, output.stdout).unwrap();
    assert_eq!(iris_nostr::io::read_events(&cache).unwrap(), vec![event]);
}

#[test]
fn config_resolves_relative_paths_and_unknown_distance_is_null() {
    let dir = TempDir::new().unwrap();
    let root = Keys::generate();
    let event = note(&root, 1, "config");
    save(&dir, "events.json", &[event]);
    save(&dir, "graph.json", &[]);
    let config = dir.path().join("profile.json");
    std::fs::write(
        &config,
        serde_json::json!({"events": ["events.json"],
        "graph": "graph.json", "identity": root.public_key().to_hex()})
        .to_string(),
    )
    .unwrap();
    let binary = env!("CARGO_BIN_EXE_iris-nostr");
    let result = std::process::Command::new(binary)
        .arg("--config")
        .arg(&config)
        .args(["events", "query", "--offline"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["events"].as_array().unwrap().len(), 1);
    let result = std::process::Command::new(binary)
        .arg("--config")
        .arg(&config)
        .args(["graph", "distance", &Keys::generate().public_key().to_hex()])
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["distance"], serde_json::Value::Null);
    assert_eq!(report["known"], false);
}

#[test]
fn cli_reports_missing_index_with_partial_exit_code() {
    let dir = TempDir::new().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_iris-nostr"))
        .args([
            "events",
            "query",
            "--offline",
            "--no-social-filter",
            "--index",
            &"ab".repeat(32),
            "--blocks",
        ])
        .arg(dir.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["partial"], true);
}

#[tokio::test]
async fn default_policy_filters_without_explicit_distance_and_optout_includes_strangers() {
    let dir = TempDir::new().unwrap();
    let owner = Keys::generate();
    let stranger = Keys::generate();
    let events = save(
        &dir,
        "posts.json",
        &[note(&owner, 1, "owner"), note(&stranger, 2, "stranger")],
    );
    let graph = save(&dir, "follows.json", &[]);
    let sources = Sources {
        events: vec![events],
        identity: Some(owner.public_key().to_hex()),
        graph: Some(graph),
        ..Default::default()
    };
    let report = query(request(sources.clone())).await.unwrap();
    assert_eq!(report["social_filter"], true);
    assert_eq!(report["max_distance"], 3);
    assert_eq!(report["events"].as_array().unwrap().len(), 1);
    let mut req = request(sources);
    req.sources.no_social_filter = true;
    let report = query(req).await.unwrap();
    assert_eq!(report["events"].as_array().unwrap().len(), 2);
}

#[test]
fn snapshot_digest_and_parser_reject_corruption() {
    let key = Keys::generate().public_key().to_hex();
    let graph = nostr_social_graph::SocialGraph::new(&key);
    let bytes = graph.to_binary().unwrap();
    let hash = hashtree_core::to_hex(&hashtree_core::sha256(&bytes));
    assert!(iris_nostr::snapshot::validate(&bytes, Some(&hash), &key).is_ok());
    assert!(iris_nostr::snapshot::validate(&bytes, Some(&"00".repeat(32)), &key).is_err());
    assert!(iris_nostr::snapshot::validate(b"not a graph", None, &key).is_err());
}

#[tokio::test]
async fn failed_snapshot_download_preserves_existing_file() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("graph.bin");
    std::fs::write(&path, b"previous").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad")
            .await
            .unwrap();
    });
    assert!(
        iris_nostr::snapshot::fetch(
            Some(&url),
            Some(&"00".repeat(32)),
            &path,
            iris_nostr::snapshot::DEFAULT_IDENTITY
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"previous");
    server.await.unwrap();
}
