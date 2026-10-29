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
