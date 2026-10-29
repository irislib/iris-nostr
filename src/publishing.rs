//! Publish already-signed events with matching relay OK evidence. No signing keys.
use anyhow::{Result, bail, ensure};
use nostr::{ClientMessage, Event, RelayMessage, RelayUrl};
use nostr_pubsub::VerifiedEvent;
use nostr_pubsub_relay::{RelaySession, RelaySessionEvent};
use serde::Serialize;
use std::{borrow::Cow, collections::HashSet, sync::Arc, time::Duration};

#[derive(Serialize)]
struct Acknowledgement {
    id: nostr::EventId,
    accepted: Option<bool>,
    message: Option<String>,
}

#[derive(Serialize)]
struct RelayReport {
    relay: String,
    events: Vec<Acknowledgement>,
    error: Option<String>,
}

async fn publish_to_relay(
    url: String,
    events: Arc<Vec<VerifiedEvent>>,
    timeout: Duration,
) -> RelayReport {
    let mut report = RelayReport {
        relay: url.clone(),
        error: None,
        events: events
            .iter()
            .map(|event| Acknowledgement {
                id: event.as_event().id,
                accepted: None,
                message: None,
            })
            .collect(),
    };
    let operation = async {
        let mut session = RelaySession::connect(&url).await?;
        while !session.is_connected() {
            match session.next().await? {
                Some(RelaySessionEvent::Connection(true)) => break,
                Some(RelaySessionEvent::Connection(false)) | None => {
                    bail!("relay connection failed")
                }
                _ => {}
            }
        }
        for event in events.iter() {
            session
                .send(ClientMessage::Event(Cow::Borrowed(event.as_event())))
                .await?;
        }
        while report.events.iter().any(|entry| entry.accepted.is_none()) {
            match session.next().await? {
                Some(RelaySessionEvent::Message(message)) => {
                    if let RelayMessage::Ok {
                        event_id,
                        status,
                        message,
                    } = *message
                        && let Some(entry) = report
                            .events
                            .iter_mut()
                            .find(|entry| entry.id == event_id && entry.accepted.is_none())
                    {
                        entry.accepted = Some(status);
                        entry.message = Some(message.into_owned());
                    }
                }
                Some(RelaySessionEvent::Connection(false)) | None => {
                    bail!("relay disconnected before all acknowledgements")
                }
                Some(RelaySessionEvent::Lagged(_)) => {
                    bail!("relay acknowledgement stream lost messages")
                }
                _ => {}
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    report.error = match tokio::time::timeout(timeout, operation).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("relay acknowledgement deadline exceeded".into()),
    };
    report
}

/// Validates the entire batch and all destinations before sending anything.
/// Dry-run performs the same validation but creates no network connection.
pub async fn publish(
    events: Vec<Event>,
    relays: Vec<String>,
    timeout: Duration,
    dry_run: bool,
) -> Result<serde_json::Value> {
    ensure!(
        (1..=100).contains(&events.len()),
        "publish requires 1..100 signed events"
    );
    ensure!(
        timeout > Duration::ZERO && timeout <= Duration::from_secs(300),
        "timeout must be 1..300 seconds"
    );
    ensure!(
        (1..=32).contains(&relays.len()),
        "publish requires 1..32 relays"
    );
    let mut seen = HashSet::new();
    let mut verified = Vec::new();
    for event in events {
        ensure!(
            serde_json::to_vec(&event)?.len() <= crate::signing::MAX_DRAFT_BYTES,
            "event exceeds 1 MiB"
        );
        let event = VerifiedEvent::try_from(event)?;
        if seen.insert(event.as_event().id) {
            verified.push(event);
        }
    }
    let mut destinations = Vec::new();
    for url in relays {
        let url = RelayUrl::parse(&url)?.to_string();
        if !destinations.contains(&url) {
            destinations.push(url);
        }
    }
    let events = Arc::new(verified);
    if dry_run {
        return Ok(
            serde_json::json!({"schema_version":1,"dry_run":true,"published":false,
            "event_count":events.len(),"relays":destinations,
            "events":events.iter().map(|e| serde_json::json!({"id":e.as_event().id,"pubkey":e.as_event().pubkey})).collect::<Vec<_>>()}),
        );
    }
    let mut jobs = tokio::task::JoinSet::new();
    for (index, url) in destinations.into_iter().enumerate() {
        let events = events.clone();
        jobs.spawn(async move { (index, publish_to_relay(url, events, timeout).await) });
    }
    let mut reports = Vec::new();
    while let Some(result) = jobs.join_next().await {
        reports.push(result?);
    }
    reports.sort_by_key(|(index, _)| *index);
    let reports = reports
        .into_iter()
        .map(|(_, report)| report)
        .collect::<Vec<_>>();
    let mut published_count = 0;
    let rows = events.iter().map(|event| {
        let accepted_by = reports.iter().filter(|report| report.events.iter().any(|entry| entry.id == event.as_event().id && entry.accepted == Some(true)))
            .map(|report| &report.relay).collect::<Vec<_>>();
        if !accepted_by.is_empty() { published_count += 1; }
        serde_json::json!({"id":event.as_event().id,"pubkey":event.as_event().pubkey,"published":!accepted_by.is_empty(),"accepted_by":accepted_by})
    }).collect::<Vec<_>>();
    let partial = reports.iter().any(|report| {
        report
            .events
            .iter()
            .any(|entry| entry.accepted != Some(true))
    });
    Ok(
        serde_json::json!({"schema_version":1,"dry_run":false,"events":rows,"relays":reports,
        "event_count":events.len(),"published_event_count":published_count,"partial":partial,
        "coverage":"matching relay OK acknowledgements; not a guarantee of permanent storage or delivery to readers"}),
    )
}
