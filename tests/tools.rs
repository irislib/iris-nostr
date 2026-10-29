use iris_nostr::{io, reference};
use nostr::{
    Event, EventBuilder, Keys, Kind, Tag, Timestamp,
    nips::{
        nip01::Coordinate,
        nip19::{Nip19Coordinate, Nip19Event, Nip19Profile, ToBech32},
    },
};
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

fn note(key: &Keys, text: &str) -> Event {
    EventBuilder::new(Kind::TextNote, text)
        .tags([Tag::parse(["t", "nostr"]).unwrap()])
        .custom_created_at(Timestamp::from(10))
        .sign_with_keys(key)
        .unwrap()
}
fn cli(args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_iris-nostr"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn public_reference_forms_preserve_metadata_and_types() {
    let key = Keys::generate().public_key();
    let event = note(&Keys::generate(), "event");
    let relay: nostr::RelayUrl = "wss://relay.example".parse().unwrap();
    let profile = Nip19Profile::new(key, [relay.clone()]).to_bech32().unwrap();
    let event_ref = Nip19Event::new(event.id)
        .author(key)
        .kind(Kind::TextNote)
        .relays([relay.clone()])
        .to_bech32()
        .unwrap();
    let addr = Nip19Coordinate::new(
        Coordinate::new(Kind::LongFormTextNote, key).identifier("article"),
        [relay],
    )
    .to_bech32()
    .unwrap();
    for value in [key.to_bech32().unwrap(), profile.clone()] {
        assert_eq!(
            reference::public_key(&format!("nostr:{value}")).unwrap(),
            key
        );
        assert_eq!(reference::decode(&value).unwrap()["pubkey"], key.to_hex());
    }
    for value in [event.id.to_bech32().unwrap(), event_ref.clone()] {
        assert_eq!(
            reference::event_id(&format!("nostr:{value}")).unwrap(),
            event.id
        );
        assert_eq!(reference::decode(&value).unwrap()["id"], event.id.to_hex());
    }
    let decoded = reference::decode(&event_ref).unwrap();
    assert_eq!(decoded["kind"], 1);
    assert_eq!(decoded["relays"][0], "wss://relay.example");
    let decoded = reference::decode(&addr).unwrap();
    assert_eq!(decoded["identifier"], "article");
    assert_eq!(decoded["kind"], 30023);
    assert!(reference::event_id(&profile).is_err());
    assert!(reference::public_key(&event_ref).is_err());
    assert!(reference::decode(&event.id.to_hex()).is_err());
}
#[test]
fn decode_rejects_secret_or_malformed_references_without_echoing_input() {
    let secret = Keys::generate().secret_key().to_bech32().unwrap();
    for input in [
        secret.clone(),
        format!("nostr:{secret}"),
        "nevent1broken".into(),
    ] {
        let out = cli(&["decode", &input], b"");
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(!error.contains(&input));
    }
}
#[test]
fn verify_accepts_signed_formats_and_rejects_all_output_on_tampering() {
    let event = note(&Keys::generate(), "valid");
    for input in [
        serde_json::to_vec(&event).unwrap(),
        serde_json::to_vec(&vec![&event]).unwrap(),
        format!("{}\n\n", serde_json::to_string(&event).unwrap()).into_bytes(),
    ] {
        let out = cli(&["events", "verify", "--jsonl"], &input);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(serde_json::from_slice::<Event>(&out.stdout).unwrap(), event);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stderr).unwrap()["event_count"],
            1
        );
    }
    let mut changed = event.clone();
    changed.content = "tampered".into();
    let out = cli(
        &["events", "verify", "--jsonl"],
        &serde_json::to_vec(&vec![event, changed]).unwrap(),
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}
#[test]
fn stdin_queries_accept_nak_jsonl_and_standard_filters_without_following_hints() {
    let key = Keys::generate();
    let selected = note(&key, "selected");
    let other = note(&key, "other");
    let hint = Nip19Event::new(selected.id)
        .author(Keys::generate().public_key())
        .kind(Kind::Metadata)
        .relays(["ws://127.0.0.1:9".parse().unwrap()])
        .to_bech32()
        .unwrap();
    let input = format!(
        "{}\n{}\n",
        serde_json::to_string(&selected).unwrap(),
        serde_json::to_string(&other).unwrap()
    );
    let out = cli(
        &[
            "events",
            "query",
            "--events",
            "-",
            "--offline",
            "--no-social-filter",
            "--id",
            &format!("nostr:{hint}"),
            "--filter",
            r##"{"kinds":[1],"#t":["nostr"],"since":10,"until":10}"##,
            "--jsonl",
        ],
        input.as_bytes(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Event>(&out.stdout).unwrap(),
        selected
    );
    let out = cli(
        &[
            "events",
            "query",
            "--events",
            "-",
            "--offline",
            "--no-social-filter",
            "--filter",
            r##"{"#t":["different"]}"##,
        ],
        input.as_bytes(),
    );
    assert!(out.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn stdin_from_config_keeps_default_graph_policy_even_for_exact_ids() {
    let dir = TempDir::new().unwrap();
    let owner = Keys::generate();
    let stranger = Keys::generate();
    let post = note(&stranger, "excluded");
    std::fs::write(dir.path().join("-"), "[]").unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::json!({"events":["-"],"graph":"-","identity":owner.public_key().to_hex()})
            .to_string(),
    )
    .unwrap();
    let out = cli(
        &[
            "--config",
            config.to_str().unwrap(),
            "events",
            "query",
            "--offline",
            "--id",
            &post.id.to_hex(),
        ],
        &serde_json::to_vec(&post).unwrap(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["social_filter"], true);
    assert!(report["events"].as_array().unwrap().is_empty());
}
#[test]
fn stdin_is_single_use_and_empty_stream_is_valid() {
    let out = cli(
        &[
            "events",
            "query",
            "--events",
            "-",
            "--events",
            "-",
            "--offline",
            "--no-social-filter",
        ],
        b"",
    );
    assert_eq!(out.status.code(), Some(1));
    let out = cli(
        &[
            "events",
            "query",
            "--events",
            "-",
            "--offline",
            "--no-social-filter",
        ],
        b"",
    );
    assert!(out.status.success());
    assert!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn bounded_reader_stops_oversize_input() {
    assert!(
        io::read_bounded_from(std::io::Read::take(
            std::io::repeat(b' '),
            io::MAX_INPUT + 1
        ))
        .is_err()
    );
}

#[tokio::test]
async fn caller_supplied_events_are_verified_before_graph_policy() {
    let mut event = note(&Keys::generate(), "original");
    event.content = "tampered".into();
    let request = iris_nostr::Request {
        sources: Default::default(),
        filter: Default::default(),
        limit: 1,
        fetch_limit: 1,
        timeout: std::time::Duration::from_secs(1),
        offline: true,
    };
    assert!(
        iris_nostr::query_with_events(request, vec![event])
            .await
            .is_err()
    );
}
