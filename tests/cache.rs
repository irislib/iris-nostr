use iris_nostr::{Request, Sources, cache, query};
use nostr::{Event, EventBuilder, Filter, Keys, Kind, Timestamp};
use serde_json::json;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    process::{Command, Output},
    time::Duration,
};
use tempfile::TempDir;

fn event(key: &Keys, kind: Kind, time: u64, content: &str) -> Event {
    EventBuilder::new(kind, content)
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(key)
        .unwrap()
}
async fn read(path: &Path) -> serde_json::Value {
    query(Request {
        sources: Sources {
            cache: Some(path.into()),
            no_social_filter: true,
            ..Default::default()
        },
        filter: Filter::new(),
        limit: 100,
        fetch_limit: 100,
        timeout: Duration::from_secs(5),
        offline: true,
        contains: Vec::new(),
    })
    .await
    .unwrap()
}
fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_iris-nostr"))
        .env_remove("IRIS_NOSTR_CONFIG")
        .args(args)
        .output()
        .unwrap()
}
fn success(out: Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[tokio::test]
async fn cache_reopens_a_real_index_and_keeps_latest_replaceable_events() {
    let dir = TempDir::new().unwrap();
    let key = Keys::generate();
    let old = event(&key, Kind::Metadata, 1, r#"{"name":"old"}"#);
    let new = event(&key, Kind::Metadata, 2, r#"{"name":"new"}"#);
    let note = event(&key, Kind::TextNote, 3, "note");
    let a = cache::merge(
        dir.path(),
        vec![old.clone(), note.clone()],
        10,
        10000,
        json!({}),
    )
    .await
    .unwrap();
    let old_reader = cache::reader(dir.path(), "old").unwrap().1;
    let b = cache::merge(
        dir.path(),
        vec![old, new.clone(), note],
        10,
        10000,
        json!({}),
    )
    .await
    .unwrap();
    assert_ne!(a.root, b.root);
    assert_eq!(b.event_count, 2);
    let report = read(dir.path()).await;
    assert_eq!(report["partial"], false);
    assert_eq!(report["events"][1]["event"]["id"], new.id.to_hex());
    use nostr_pubsub::{EventBus, QueryOptions};
    assert_eq!(
        old_reader
            .query(vec![Filter::new()], QueryOptions { limit: Some(10) })
            .await
            .unwrap()
            .events
            .len(),
        2
    );
    let c = cache::merge(dir.path(), vec![new], 1, 10000, json!({}))
        .await
        .unwrap();
    assert_eq!(c.event_count, 1);
    assert_eq!(c.newest_event_at, Some(3));
    let preserved = cache::merge_preserving_limits(dir.path(), vec![], json!({}))
        .await
        .unwrap();
    assert_eq!(preserved.max_events, 1);
    assert_eq!(preserved.max_bytes, 10000);
}
#[tokio::test]
async fn cache_ties_bytes_empty_and_writer_lock_are_predictable() {
    let dir = TempDir::new().unwrap();
    let key = Keys::generate();
    let a = event(&key, Kind::Metadata, 2, "a");
    let b = event(&key, Kind::Metadata, 2, "b");
    let expected = std::cmp::min(a.id, b.id).to_hex();
    cache::merge(dir.path(), vec![a, b], 10, 10000, json!({}))
        .await
        .unwrap();
    assert_eq!(read(dir.path()).await["events"][0]["event"]["id"], expected);
    let before = fs::read(dir.path().join("state.json")).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join(".writer.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(
        cache::merge(dir.path(), vec![], 10, 10000, json!({}))
            .await
            .is_err()
    );
    assert_eq!(fs::read(dir.path().join("state.json")).unwrap(), before);
    drop(lock);
    let empty = cache::merge(dir.path(), vec![], 10, 1, json!({}))
        .await
        .unwrap();
    assert_eq!(empty.event_count, 0);
    assert!(empty.root.is_none());
    assert_eq!(read(dir.path()).await["events"], json!([]));
}
#[tokio::test]
async fn corrupt_cache_and_invalid_inputs_fail_without_replacing_checkpoint() {
    use hashtree_core::to_hex;
    let dir = TempDir::new().unwrap();
    let key = Keys::generate();
    let note = event(&key, Kind::TextNote, 3, "note");
    let state = cache::merge(dir.path(), vec![note.clone()], 10, 10000, json!({}))
        .await
        .unwrap();
    let before = fs::read(dir.path().join("state.json")).unwrap();
    let mut forged = note;
    forged.content = "forged".into();
    assert!(
        cache::merge(dir.path(), vec![forged], 10, 10000, json!({}))
            .await
            .is_err()
    );
    let cid = iris_nostr::parse_root(state.root.as_ref().unwrap()).unwrap();
    let hash = to_hex(&cid.hash);
    let block = dir
        .path()
        .join("blobs")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash[4..]);
    assert!(block.exists(), "{}", block.display());
    fs::write(&block, b"corrupt").unwrap();
    assert_eq!(read(dir.path()).await["partial"], true);
    assert!(
        cache::merge(dir.path(), vec![], 10, 10000, json!({}))
            .await
            .is_err()
    );
    fs::remove_file(block).unwrap();
    assert_eq!(read(dir.path()).await["partial"], true);
    assert_eq!(fs::read(dir.path().join("state.json")).unwrap(), before);
    let foreign = TempDir::new().unwrap();
    fs::write(foreign.path().join("app.db"), b"do not touch").unwrap();
    assert!(
        cache::merge(foreign.path(), vec![], 10, 10000, json!({}))
            .await
            .is_err()
    );
    assert!(!foreign.path().join(".writer.lock").exists());
}
#[test]
fn sync_profiles_and_shared_relative_configuration_work_offline() {
    let dir = TempDir::new().unwrap();
    let key = Keys::generate();
    let pk = key.public_key().to_hex();
    let profile = event(&key, Kind::Metadata, 2, r#"{"name":"public name"}"#);
    fs::write(
        dir.path().join("input.json"),
        serde_json::to_vec(&vec![profile]).unwrap(),
    )
    .unwrap();
    let config = dir.path().join("sources.json");
    fs::write(
        &config,
        br#"{"cache":"cache","events":["input.json"],"no_social_filter":true}"#,
    )
    .unwrap();
    let cfg = config.to_str().unwrap();
    let sources = Sources::from_file(&config).unwrap();
    assert_eq!(sources.cache.unwrap(), dir.path().join("cache"));
    let result = success(cli(&["--config", cfg, "events", "sync", "--offline"]));
    assert_eq!(result["cache_update"]["state"]["event_count"], 1);
    let result = success(cli(&[
        "--config",
        cfg,
        "profiles",
        "refresh",
        &pk,
        "--offline",
    ]));
    assert_eq!(result["profile"]["metadata"]["name"], "public name");
    // An empty refresh cannot erase already observed metadata.
    fs::write(dir.path().join("input.json"), b"[]").unwrap();
    let result = success(cli(&[
        "--config",
        cfg,
        "profiles",
        "refresh",
        &pk,
        "--offline",
    ]));
    assert_eq!(result["profile"]["metadata"]["name"], "public name");
    let status = success(cli(&["--config", cfg, "cache", "status"]));
    assert_eq!(status["state"]["event_count"], 1);
    let before = fs::read(dir.path().join("cache/state.json")).unwrap();
    let failed = cli(&[
        "--config",
        cfg,
        "events",
        "sync",
        "--relay",
        "ws://127.0.0.1:1",
        "--timeout",
        "1",
    ]);
    assert_eq!(failed.status.code(), Some(3));
    assert_eq!(
        fs::read(dir.path().join("cache/state.json")).unwrap(),
        before
    );
    fs::write(&config, br#"{"cache":"cache","no_social_filter":true}"#).unwrap();
    let result = success(cli(&["--config", cfg, "profiles", "get", &pk, "--offline"]));
    assert_eq!(result["profile"]["metadata"]["name"], "public name");
    let result = success(
        Command::new(env!("CARGO_BIN_EXE_iris-nostr"))
            .env("IRIS_NOSTR_CONFIG", cfg)
            .args(["config"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        result["sources"]["cache"],
        dir.path().join("cache").to_str().unwrap()
    );
}
#[test]
fn graph_refresh_uses_existing_daemon_snapshot_and_preserves_on_bad_digest() {
    let dir = TempDir::new().unwrap();
    let key = Keys::generate();
    let identity = key.public_key().to_hex();
    let input = dir.path().join("follows.json");
    fs::write(
        &input,
        serde_json::to_vec(
            &EventBuilder::new(Kind::ContactList, "")
                .tags([nostr::Tag::public_key(Keys::generate().public_key())])
                .custom_created_at(Timestamp::from(7))
                .sign_with_keys(&key)
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    let graph = iris_nostr::graph::load_graph(&identity, &input).unwrap();
    let bytes = graph.to_binary().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let count = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..count])
                    .starts_with("GET /api/socialgraph/snapshot ")
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        }
    });
    let path = dir.path().join("graph.bin");
    let out = path.to_str().unwrap();
    success(cli(&[
        "graph",
        "refresh",
        "--daemon",
        &address,
        "--output",
        out,
        "--identity",
        &identity,
    ]));
    let before = fs::read(&path).unwrap();
    let status = success(cli(&[
        "graph",
        "status",
        "--graph",
        out,
        "--identity",
        &identity,
    ]));
    assert_eq!(status["root_follow_list_created_at"], 7);
    assert_ne!(
        status["local_file_modified_at"],
        status["root_follow_list_created_at"]
    );
    assert_eq!(
        cli(&[
            "graph",
            "refresh",
            "--daemon",
            &address,
            "--output",
            out,
            "--identity",
            &identity,
            "--sha256",
            &"0".repeat(64)
        ])
        .status
        .code(),
        Some(1)
    );
    assert_eq!(fs::read(path).unwrap(), before);
    server.join().unwrap();
}
