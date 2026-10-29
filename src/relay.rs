use async_trait::async_trait;
use nostr::{ClientMessage, RelayMessage, RelayUrl, SubscriptionId};
use nostr_pubsub::*;
use nostr_pubsub_relay::{RelaySession, RelaySessionEvent};
use std::collections::HashSet;

pub struct RelaySource {
    url: String,
    id: String,
}
impl RelaySource {
    pub fn new(url: &str, id: &str) -> anyhow::Result<Self> {
        let parsed = RelayUrl::parse(url)?;
        Ok(Self {
            url: parsed.to_string(),
            id: id.into(),
        })
    }
}

#[async_trait]
impl EventBus for RelaySource {
    async fn publish(&self, _: VerifiedEvent, _: EventSource) -> Result<PublishReport> {
        Err(PubsubError::Validation("read-only source".into()))
    }
    async fn query(&self, filters: Vec<Filter>, options: QueryOptions) -> Result<QueryReport> {
        // Shared transport handles reconnection and verification. The CLI owns
        // bounded-query semantics: a timeout/disconnect is never an empty success.
        let mut session = RelaySession::connect(&self.url).await?;
        let subscription = SubscriptionId::generate();
        session
            .send(ClientMessage::req(subscription.clone(), filters.clone()))
            .await?;
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        loop {
            match session.next().await? {
                Some(RelaySessionEvent::Message(message)) => match *message {
                    RelayMessage::Event {
                        subscription_id,
                        event,
                    } if *subscription_id == subscription => {
                        if filters
                            .iter()
                            .any(|filter| filter.match_event(&event, Default::default()))
                            && seen.insert(event.id)
                        {
                            events.push(QueryEvent {
                                event: VerifiedEvent::try_from(event.into_owned())?,
                                source: EventSource::relay(&self.id),
                                priority: SOURCE_PRIORITY_RELAY,
                            });
                            if events.len() >= options.limit.unwrap_or(1000) {
                                break;
                            }
                        }
                    }
                    RelayMessage::EndOfStoredEvents(id) if *id == subscription => break,
                    RelayMessage::Closed {
                        subscription_id, ..
                    } if *subscription_id == subscription => {
                        return Err(PubsubError::Storage(
                            "relay closed the query before EOSE".into(),
                        ));
                    }
                    _ => {}
                },
                Some(RelaySessionEvent::Connection(false)) | None => {
                    return Err(PubsubError::Storage(
                        "relay disconnected before EOSE".into(),
                    ));
                }
                Some(RelaySessionEvent::Lagged(_)) => {
                    return Err(PubsubError::Storage("relay query lost events".into()));
                }
                _ => {}
            }
        }
        events.sort_by(|a, b| {
            b.event
                .as_event()
                .created_at
                .cmp(&a.event.as_event().created_at)
                .then_with(|| a.event.as_event().id.cmp(&b.event.as_event().id))
        });
        Ok(QueryReport { events })
    }
}
