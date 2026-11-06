use iris_nostr::{publishing, signing};
use nostr::{Event, EventBuilder, Keys, Kind, Timestamp, nips::nip19::ToBech32};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

fn event(key: &Keys, time: u64) -> Event {
    EventBuilder::new(Kind::TextNote, "synthetic test")
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(key)
        .unwrap()
}
fn draft(pubkey: Option<nostr::PublicKey>) -> signing::Draft {
    signing::Draft {
        kind: 1,
        content: "test".into(),
        tags: vec![vec!["t".into(), "test".into()]],
        created_at: Some(1),
        pubkey,
        id: None,
    }
}
fn cli(args: &[&str], input: &[u8]) -> std::process::Output {
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
fn shared_identity_selection_is_explicit_and_never_exports_secrets() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("keys");
    let a = Keys::generate();
    let b = Keys::generate();
    let secret = a.secret_key().to_bech32().unwrap();
    std::fs::write(
        &path,
        format!(
            "# Hashtree keys\n{secret} work\n{} observer\n",
            b.public_key().to_bech32().unwrap()
        ),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let public = serde_json::to_string(&signing::identities(&path).unwrap()).unwrap();
    assert!(!public.contains(&secret));
    assert!(!public.contains("nsec1"));
    let signed = signing::sign_from_store(draft(None), &path, "work").unwrap();
    signed.verify().unwrap();
    assert_eq!(signed.pubkey, a.public_key());
    assert_eq!(
        signing::sign_from_store(draft(None), &path, &a.public_key().to_bech32().unwrap())
            .unwrap()
            .id,
        signed.id
    );
    for selector in ["absent", "observer", secret.as_str()] {
        let err = signing::sign_from_store(draft(None), &path, selector)
            .unwrap_err()
            .to_string();
        assert!(!err.contains(&secret));
    }
    assert!(signing::sign_from_store(draft(Some(b.public_key())), &path, "work").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::write(
        &path,
        format!(
            "{secret} work\n{} work\n",
            b.secret_key().to_bech32().unwrap()
        ),
    )
    .unwrap();
    assert!(
        signing::sign_from_store(draft(None), &path, "work")
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    std::fs::write(&path, format!("{secret} {secret}\n")).unwrap();
    let error = signing::identities(&path).err().unwrap().to_string();
    assert!(!error.contains(&secret));
}

#[test]
fn signer_cli_roundtrip_and_dry_run_need_no_network_or_graph() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("keys");
    let keys = Keys::generate();
    let secret = keys.secret_key().to_bech32().unwrap();
    std::fs::write(&path, format!("{secret} agent\n")).unwrap();
    let config = dir.path().join("sources.json");
    std::fs::write(
        &config,
        json!({"identity":Keys::generate().public_key().to_hex(),"graph":"missing.bin"})
            .to_string(),
    )
    .unwrap();
    let out = cli(
        &[
            "--config",
            config.to_str().unwrap(),
            "events",
            "sign",
            "--signer",
            "agent",
            "--keys-file",
            path.to_str().unwrap(),
        ],
        br#"{"kind":1,"content":"test","created_at":1}"#,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&secret));
    let signed: Event = serde_json::from_slice(&out.stdout).unwrap();
    signed.verify().unwrap();
    assert_eq!(signed.pubkey, keys.public_key());
    let output = cli(
        &[
            "events",
            "publish",
            "--dry-run",
            "--relay",
            "wss://example.invalid",
        ],
        &out.stdout,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["published"], false);
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["events"][0]["id"], signed.id.to_hex());
}

#[test]
fn draft_rejects_unknown_fields_wrong_ids_and_oversized_input() {
    assert!(
        signing::read_draft(br#"{"kind":1,"content":"test","secret":"not allowed"}"#.as_slice())
            .is_err()
    );
    let keys = Keys::generate();
    let mut value = draft(None);
    value.id = Some(event(&keys, 2).id);
    assert!(signing::sign(value, &keys).is_err());
    assert!(signing::read_draft(vec![b' '; signing::MAX_DRAFT_BYTES + 1].as_slice()).is_err());
}

async fn mock_relay(statuses: Vec<Option<bool>>) -> (String, tokio::task::JoinHandle<()>) {
    mock_relay_with_saved_event(statuses, None).await
}

async fn mock_relay_with_saved_event(
    statuses: Vec<Option<bool>>,
    saved: Option<std::path::PathBuf>,
) -> (String, tokio::task::JoinHandle<()>) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let mut index = 0;
        while let Some(Ok(frame)) = ws.next().await {
            let Ok(value) = serde_json::from_slice::<Value>(&frame.into_data()) else {
                continue;
            };
            if value[0] != "EVENT" {
                continue;
            }
            let received: Event = serde_json::from_value(value[1].clone()).unwrap();
            received.verify().unwrap();
            if let Some(path) = &saved {
                let recovery: Event =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                assert_eq!(recovery, received, "save the exact event before sending it");
            }
            // A positive acknowledgement for a different event must be ignored.
            ws.send(Message::Text(
                json!(["OK", "00".repeat(32), true, ""]).to_string().into(),
            ))
            .await
            .unwrap();
            if let Some(status) = statuses[index] {
                ws.send(Message::Text(
                    json!([
                        "OK",
                        received.id,
                        status,
                        if status {
                            ""
                        } else {
                            "blocked: synthetic test"
                        }
                    ])
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            }
            index += 1;
        }
    });
    (url, task)
}

#[tokio::test]
async fn publishing_reports_acknowledgements_rejections_and_partial_delivery() {
    let keys = Keys::generate();
    let events = vec![event(&keys, 1), event(&keys, 2)];
    let (a, first) = mock_relay(vec![Some(true), Some(true)]).await;
    let (b, second) = mock_relay(vec![Some(true), Some(false)]).await;
    let report = publishing::publish(events, vec![a, b], Duration::from_secs(2), false)
        .await
        .unwrap();
    assert_eq!(report["published_event_count"], 2);
    assert_eq!(report["partial"], true);
    assert_eq!(
        report["events"][0]["accepted_by"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        report["events"][1]["accepted_by"].as_array().unwrap().len(),
        1
    );
    assert_eq!(report["relays"][1]["events"][1]["accepted"], false);
    first.abort();
    second.abort();
}

#[tokio::test]
async fn missing_ack_is_not_success_and_keeps_earlier_acknowledgements() {
    let keys = Keys::generate();
    let (url, server) = mock_relay(vec![Some(true), None]).await;
    let report = publishing::publish(
        vec![event(&keys, 1), event(&keys, 2)],
        vec![url],
        Duration::from_millis(200),
        false,
    )
    .await
    .unwrap();
    assert_eq!(report["published_event_count"], 1);
    assert_eq!(report["partial"], true);
    assert_eq!(report["relays"][0]["events"][0]["accepted"], true);
    assert_eq!(report["relays"][0]["events"][1]["accepted"], Value::Null);
    assert!(
        report["relays"][0]["error"]
            .as_str()
            .unwrap()
            .contains("deadline")
    );
    server.abort();
}

#[tokio::test]
async fn invalid_batch_or_destination_never_opens_a_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let good = event(&Keys::generate(), 1);
    let mut forged = good.clone();
    forged.content = "tampered".into();
    assert!(
        publishing::publish(
            vec![good.clone(), forged],
            vec![url.clone()],
            Duration::from_secs(1),
            false
        )
        .await
        .is_err()
    );
    assert!(
        publishing::publish(
            vec![good],
            vec![url, "invalid relay".into()],
            Duration::from_secs(1),
            false
        )
        .await
        .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn acknowledged_batch_succeeds_and_duplicate_input_is_sent_once() {
    let original = event(&Keys::generate(), 1);
    let (url, server) = mock_relay(vec![Some(true)]).await;
    let report = publishing::publish(
        vec![original.clone(), original],
        vec![url],
        Duration::from_secs(2),
        false,
    )
    .await
    .unwrap();
    assert_eq!(report["event_count"], 1);
    assert_eq!(report["published_event_count"], 1);
    assert_eq!(report["partial"], false);
    server.abort();
}

#[tokio::test]
async fn publish_cli_exit_codes_distinguish_acceptance_partial_and_rejection() {
    for (statuses, expected) in [(vec![true], 0), (vec![true, false], 3), (vec![false], 1)] {
        let mut relays = Vec::new();
        let mut servers = Vec::new();
        for status in statuses {
            let (url, server) = mock_relay(vec![Some(status)]).await;
            relays.push(url);
            servers.push(server);
        }
        let input = serde_json::to_vec(&event(&Keys::generate(), 1)).unwrap();
        let output = tokio::task::spawn_blocking(move || {
            let mut args = vec!["events", "publish"];
            for relay in &relays {
                args.extend(["--relay", relay.as_str()]);
            }
            cli(&args, &input)
        })
        .await
        .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["published_event_count"],
            if expected == 1 { 0 } else { 1 }
        );
        for server in servers {
            server.abort();
        }
    }
}

struct PostFixture {
    dir: TempDir,
    keys: Keys,
}
impl PostFixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let keys = Keys::generate();
        std::fs::write(
            dir.path().join("keys"),
            format!("{} bot\n", keys.secret_key().to_bech32().unwrap()),
        )
        .unwrap();
        Self { dir, keys }
    }

    fn publish(&self, args: &[&str], text: &[u8]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_iris-nostr"));
        command.arg("publish");
        if !args.contains(&"--signer") {
            command.args(["--signer", "bot"]);
        }
        let mut child = command
            .env_remove("IRIS_NOSTR_CONFIG")
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .arg("--keys-file")
            .arg(self.dir.path().join("keys"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Invalid arguments may close stdin before consuming input.
        let _ = child.stdin.take().unwrap().write_all(text);
        child.wait_with_output().unwrap()
    }
}

#[test]
fn text_publish_dry_run_saves_exact_text_and_explicit_signer() {
    let fixture = PostFixture::new();
    let before = std::fs::read(fixture.dir.path().join("keys")).unwrap();
    let content = "Hello Nostr! 🌳\nSecond line\n";
    let out = fixture.publish(
        &["--dry-run", "--relay", "wss://example.invalid"],
        content.as_bytes(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["published"], false);
    let saved = std::path::Path::new(report["signed_event_file"].as_str().unwrap());
    assert!(saved.starts_with(fixture.dir.path().join("data/iris-nostr/signed")));
    let signed: Event = serde_json::from_slice(&std::fs::read(saved).unwrap()).unwrap();
    signed.verify().unwrap();
    assert_eq!(signed.kind, Kind::TextNote);
    assert_eq!(signed.content, content);
    assert_eq!(signed.pubkey, fixture.keys.public_key());
    assert_eq!(report["events"][0]["id"], signed.id.to_hex());
    assert_eq!(
        std::fs::read(fixture.dir.path().join("keys")).unwrap(),
        before
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout)
            .contains(&fixture.keys.secret_key().to_bech32().unwrap())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(saved).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn text_publish_validates_and_saves_before_connecting() {
    let fixture = PostFixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let saved = fixture.dir.path().join("saved.json");
    let path = saved.to_str().unwrap();
    for (extra, content, error) in [
        (vec!["--relay", "invalid"], b"hello".as_slice(), None),
        (vec!["--timeout", "0"], b"hello".as_slice(), Some("timeout")),
        (vec![], b" \n".as_slice(), Some("empty")),
        (vec![], b"\xff".as_slice(), Some("UTF-8")),
        (
            vec!["--signer", "missing"],
            b"hello".as_slice(),
            Some("signer not found"),
        ),
    ] {
        let mut args = vec!["--relay", &url, "--save-signed", path];
        args.extend(extra);
        let out = fixture.publish(&args, content);
        assert_eq!(out.status.code(), Some(1));
        if let Some(error) = error {
            assert!(String::from_utf8_lossy(&out.stderr).contains(error));
        }
        assert!(!saved.exists());
    }
    let out = fixture.publish(
        &["--relay", &url, "--save-signed", path],
        &vec![b'x'; signing::MAX_DRAFT_BYTES + 1],
    );
    assert!(!out.status.success());
    assert!(!saved.exists());
    std::fs::write(&saved, "preserve existing file").unwrap();
    let out = fixture.publish(&["--relay", &url, "--save-signed", path], b"hello");
    assert!(!out.status.success());
    assert_eq!(
        std::fs::read_to_string(&saved).unwrap(),
        "preserve existing file"
    );
    let missing = fixture.dir.path().join("missing/event.json");
    let out = fixture.publish(
        &["--relay", &url, "--save-signed", missing.to_str().unwrap()],
        b"hello",
    );
    assert!(!out.status.success());
    let dry = fixture.dir.path().join("dry.json");
    let out = fixture.publish(
        &[
            "--relay",
            &url,
            "--save-signed",
            dry.to_str().unwrap(),
            "--dry-run",
        ],
        b"offline note",
    );
    assert!(out.status.success());
    assert!(dry.exists());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn text_publish_requires_a_signer_and_honors_configured_relays_and_overrides() {
    let missing = cli(&["publish", "--dry-run"], b"");
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--signer"));
    let fixture = PostFixture::new();
    let config = fixture.dir.path().join("sources.json");
    std::fs::write(
        &config,
        json!({
            "relays": ["wss://configured.example.invalid"],
            "identity": Keys::generate().public_key().to_hex(),
            "graph": "missing.bin"
        })
        .to_string(),
    )
    .unwrap();
    for explicit in [false, true] {
        let output = fixture.dir.path().join(format!("{explicit}.json"));
        let mut args = vec![
            "--config",
            config.to_str().unwrap(),
            "--dry-run",
            "--save-signed",
            output.to_str().unwrap(),
        ];
        let expected = if explicit {
            args.extend(["--relay", "wss://override.example.invalid"]);
            "wss://override.example.invalid"
        } else {
            "wss://configured.example.invalid"
        };
        let out = fixture.publish(&args, b"synthetic configured post");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            report["relays"],
            json!([nostr::RelayUrl::parse(expected).unwrap().to_string()])
        );
        assert_eq!(
            report["events"][0]["pubkey"],
            fixture.keys.public_key().to_hex()
        );
    }
}

#[tokio::test]
async fn text_publish_reports_delivery_and_retries_saved_event_without_keys() {
    for (statuses, expected) in [
        (vec![Some(true)], 0),
        (vec![Some(true), Some(false)], 3),
        (vec![Some(false)], 1),
        (vec![None], 1),
    ] {
        let fixture = PostFixture::new();
        let saved = fixture.dir.path().join("saved.json");
        let mut relays = Vec::new();
        let mut servers = Vec::new();
        for status in statuses {
            let (url, server) =
                mock_relay_with_saved_event(vec![status], Some(saved.clone())).await;
            relays.push(url);
            servers.push(server);
        }
        let save_path = saved.clone();
        let (fixture, out) = tokio::task::spawn_blocking(move || {
            let mut args = vec![
                "--save-signed",
                save_path.to_str().unwrap(),
                "--timeout",
                "1",
            ];
            for relay in &relays {
                args.extend(["--relay", relay]);
            }
            let out = fixture.publish(&args, b"synthetic text post");
            (fixture, out)
        })
        .await
        .unwrap();
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        let original = std::fs::read(&saved).unwrap();
        let event: Event = serde_json::from_slice(&original).unwrap();
        assert_eq!(report["events"][0]["id"], event.id.to_hex());
        assert_eq!(report["partial"], expected != 0);
        assert_eq!(report["signed_event_file"], saved.to_str().unwrap());
        std::fs::remove_file(fixture.dir.path().join("keys")).unwrap();
        let (url, retry_server) = mock_relay(vec![Some(true)]).await;
        let out = tokio::task::spawn_blocking(move || {
            cli(
                &[
                    "events",
                    "publish",
                    "--events",
                    saved.to_str().unwrap(),
                    "--relay",
                    &url,
                ],
                b"",
            )
        })
        .await
        .unwrap();
        assert!(out.status.success());
        let retry: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(retry["events"][0]["id"], event.id.to_hex());
        for server in servers {
            server.abort();
        }
        retry_server.abort();
    }
}
