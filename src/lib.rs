//! Model-independent read API. No signing keys, UI types, or messaging state.
pub mod blocks;
pub mod graph;
pub mod io;
mod relay;

use anyhow::{Result, ensure};
use async_trait::async_trait;
use hashtree_core::{Cid, nhash_decode};
use hashtree_nostr_pubsub::HashtreeNostrIndexEventBus;
use nostr::Filter;
use nostr_pubsub::*;
use nostr_pubsub_social_graph::{GraphDistanceAction, SocialGraphPolicy, SocialGraphPolicyConfig};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub events: Vec<PathBuf>,
    pub indexes: Vec<String>,
    pub blocks: Option<PathBuf>,
    pub daemon: Option<String>,
    pub relays: Vec<String>,
    pub identity: Option<String>,
    pub graph: Option<PathBuf>,
    pub max_distance: Option<u32>,
}

pub struct Request {
    pub sources: Sources,
    pub filter: Filter,
    pub limit: usize,
    pub fetch_limit: usize,
    pub timeout: Duration,
    pub offline: bool,
}

struct AcceptAll;
#[async_trait]
impl PubsubPolicy for AcceptAll {
    async fn check_event(&self, _: EventPolicyContext<'_>) -> nostr_pubsub::Result<PolicyDecision> {
        Ok(PolicyDecision::allow_with_priority(0))
    }
    async fn check_source(
        &self,
        _: SourcePolicyContext<'_>,
    ) -> nostr_pubsub::Result<PolicyDecision> {
        Ok(PolicyDecision::allow_with_priority(0))
    }
}

/// The router selects sources; this boundary applies author policy to every event.
struct CheckedSource {
    bus: Arc<dyn EventBus>,
    policy: Arc<dyn PubsubPolicy>,
    timeout: Duration,
    fetch_limit: usize,
}
#[async_trait]
impl EventBus for CheckedSource {
    async fn publish(
        &self,
        _: VerifiedEvent,
        _: EventSource,
    ) -> nostr_pubsub::Result<PublishReport> {
        Err(PubsubError::Validation("read-only source".into()))
    }
    async fn query(
        &self,
        filters: Vec<Filter>,
        options: QueryOptions,
    ) -> nostr_pubsub::Result<QueryReport> {
        let mut report = tokio::time::timeout(
            self.timeout,
            self.bus.query(
                filters,
                QueryOptions {
                    limit: Some(self.fetch_limit),
                },
            ),
        )
        .await
        .map_err(|_| PubsubError::Storage("source deadline exceeded".into()))??;
        let mut accepted = Vec::new();
        for row in report.events.drain(..) {
            if !matches!(
                self.policy
                    .check_event(EventPolicyContext {
                        event: &row.event,
                        source: &row.source
                    })
                    .await?,
                PolicyDecision::Drop { .. }
            ) {
                accepted.push(row);
            }
        }
        accepted.truncate(options.limit.unwrap_or(self.fetch_limit));
        Ok(QueryReport { events: accepted })
    }
}

pub fn parse_root(value: &str) -> Result<Cid> {
    if value.starts_with("nhash1") {
        let root = nhash_decode(value)?;
        Ok(Cid {
            hash: root.hash,
            key: root.decrypt_key,
        })
    } else {
        Ok(Cid::parse(value)?)
    }
}

pub async fn query(request: Request) -> Result<serde_json::Value> {
    ensure!(
        (1..=10_000).contains(&request.limit),
        "limit must be 1..10000"
    );
    ensure!(
        (request.limit..=10_000).contains(&request.fetch_limit),
        "fetch-limit must be between limit and 10000"
    );
    ensure!(
        request.timeout > Duration::ZERO && request.timeout <= Duration::from_secs(300),
        "timeout must be 1..300 seconds"
    );
    let sources = &request.sources;
    ensure!(
        request.filter.search.is_none() || sources.indexes.is_empty(),
        "Hashtree event indexes do not support NIP-50 search"
    );
    ensure!(
        !request.offline || (sources.relays.is_empty() && sources.daemon.is_none()),
        "offline mode forbids relays and the daemon (which may fetch from peers)"
    );
    ensure!(
        !sources.events.is_empty()
            || !sources.indexes.is_empty()
            || !sources.relays.is_empty()
            || sources.daemon.is_some(),
        "choose an events file, index, relay, or daemon"
    );
    ensure!(
        sources.indexes.is_empty() || sources.blocks.is_some() || sources.daemon.is_some(),
        "indexes require --blocks or --daemon"
    );
    ensure!(
        sources.max_distance.is_none() || sources.graph.is_some(),
        "max-distance requires a graph snapshot"
    );
    let policy: Arc<dyn PubsubPolicy> = if let Some(path) = &sources.graph {
        let identity = sources
            .identity
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("graph requires a public identity"))?;
        Arc::new(SocialGraphPolicy::new(
            Arc::new(RwLock::new(graph::load_graph(identity, path)?)),
            SocialGraphPolicyConfig {
                max_follow_distance: sources.max_distance,
                outside_graph_action: if sources.max_distance.is_some() {
                    GraphDistanceAction::Drop
                } else {
                    GraphDistanceAction::Allow
                },
                // Only explicit root mutes by default; heuristic overmute is not user intent.
                drop_overmuted: false,
                ..Default::default()
            },
        ))
    } else {
        Arc::new(AcceptAll)
    };
    if let Some(identity) = &sources.identity {
        graph::public_key(identity)?;
    }
    let daemon = sources
        .daemon
        .as_deref()
        .map(blocks::daemon_url)
        .transpose()?;
    let mut buses: Vec<(SourceRoute, Arc<dyn EventBus>)> = Vec::new();
    for (i, path) in sources.events.iter().enumerate() {
        let id = format!("file-{i}");
        let bus = Arc::new(InMemoryEventBus::new());
        for event in io::read_events(path)? {
            bus.publish(
                VerifiedEvent::try_from(event)?,
                EventSource::local_index(&id),
            )
            .await?;
        }
        buses.push((SourceRoute::local_index(&id).with_dataset(&id)?, bus));
    }
    let store = Arc::new(blocks::Blocks::new(
        sources.blocks.clone(),
        daemon.clone(),
        request.timeout,
    )?);
    for (i, root) in sources.indexes.iter().enumerate() {
        let id = format!("index-{i}");
        let bus = Arc::new(HashtreeNostrIndexEventBus::new(
            store.clone(),
            Some(parse_root(root)?),
            EventSource::local_index(&id),
        ));
        buses.push((SourceRoute::local_index(&id).with_dataset(&id)?, bus));
    }
    let mut relay_urls = sources.relays.clone();
    // With explicit roots, the daemon supplies blocks only. Otherwise use its Nostr interface.
    if sources.indexes.is_empty()
        && let Some(mut url) = daemon
    {
        url.set_scheme("ws")
            .map_err(|_| anyhow::anyhow!("invalid daemon URL"))?;
        url.set_path("/ws");
        relay_urls.push(url.to_string());
    }
    for (i, url) in relay_urls.iter().enumerate() {
        let id = format!("relay-{i}");
        buses.push((
            SourceRoute::relay(&id).with_dataset(&id)?,
            Arc::new(relay::RelaySource::new(url, &id)?),
        ));
    }
    let mut router = NostrPubsubRouter::new(Arc::new(AcceptAll));
    for (route, bus) in buses {
        router = router.with_query_source(RouterQuerySource::new(
            route,
            Arc::new(CheckedSource {
                bus,
                policy: policy.clone(),
                timeout: request.timeout,
                fetch_limit: request.fetch_limit,
            }),
        ));
    }
    let mut filter = request.filter;
    filter.limit = Some(
        filter
            .limit
            .unwrap_or(request.fetch_limit)
            .min(request.fetch_limit),
    );
    let report = router
        .query_with_context(
            vec![filter],
            RoutedQueryOptions {
                query: QueryOptions {
                    limit: Some(request.limit),
                },
            },
            None,
            None,
        )
        .await;
    let report = report?;
    let events = report
        .events
        .iter()
        .map(|row| {
            serde_json::json!({
                "event": row.event.as_event(),
                "sources": row.provenance.iter().map(|p| p.route_id.clone()).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let attempts = report.attempts.iter().map(|attempt| match &attempt.outcome {
        RouteAttemptOutcome::Success { event_count } => serde_json::json!({"source": attempt.route.id, "ok": true, "event_count": event_count}),
        RouteAttemptOutcome::Failure { message } => serde_json::json!({"source": attempt.route.id, "ok": false, "error": message}),
    }).collect::<Vec<_>>();
    Ok(serde_json::json!({
        "schema_version": 1, "events": events, "sources": attempts,
        "partial": !report.complete,
        "coverage": "bounded; source success does not prove complete Nostr history",
        "fetch_limit_per_source": request.fetch_limit,
        "identity": sources.identity.as_deref().map(graph::public_key).transpose()?.map(|key| key.to_hex()),
    }))
}
