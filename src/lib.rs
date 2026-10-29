//! Model-independent Nostr queries and explicit signing/publishing operations.
pub mod blocks;
pub mod cache;
pub mod discovery;
pub mod graph;
pub mod io;
pub mod publishing;
pub mod reference;
mod relay;
pub mod signing;
pub mod snapshot;

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
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

pub const DEFAULT_MAX_DISTANCE: u32 = 3;
/// CLI fallback for online discovery without an explicitly selected source.
pub const DEFAULT_RELAYS: &[&str] = &[
    "wss://temp.iris.to",
    "wss://vault.iris.to",
    "wss://relay.primal.net",
    "wss://relay.ditto.pub",
];

// Keep generated REQs small. Larger graphs are enforced locally within the
// same candidate budget; user-supplied Nostr filters remain intact.
const MAX_PUSHED_GRAPH_AUTHORS: usize = 200;

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub cache: Option<PathBuf>,
    pub events: Vec<PathBuf>,
    pub indexes: Vec<String>,
    pub blocks: Option<PathBuf>,
    pub daemon: Option<String>,
    pub relays: Vec<String>,
    pub identity: Option<String>,
    pub graph: Option<PathBuf>,
    pub max_distance: Option<u32>,
    pub no_social_filter: bool,
}

impl Sources {
    /// Public source profile; usable by native consumers without CLI types.
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let mut sources: Self = serde_json::from_slice(&io::read_bounded(path)?)?;
        let base = path.parent().unwrap_or(std::path::Path::new("."));
        for entry in &mut sources.events {
            if entry.is_relative() && entry != std::path::Path::new("-") {
                *entry = base.join(&*entry);
            }
        }
        for entry in sources
            .blocks
            .iter_mut()
            .chain(sources.graph.iter_mut())
            .chain(sources.cache.iter_mut())
        {
            if entry.is_relative() {
                *entry = base.join(&*entry);
            }
        }
        Ok(sources)
    }
}

pub struct Request {
    pub sources: Sources,
    pub filter: Filter,
    pub limit: usize,
    pub fetch_limit: usize,
    pub timeout: Duration,
    pub offline: bool,
    /// Case-insensitive alternatives over content and descriptive tags.
    pub contains: Vec<String>,
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
    contains: Arc<Vec<String>>,
    stats: Arc<Mutex<Option<SourceStats>>>,
}

#[derive(Default, Serialize)]
struct SourceStats {
    fetched: usize,
    graph_rejected: usize,
    text_rejected: usize,
    matched: usize,
    candidate_limit_reached: bool,
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
        let candidate_limit = filters
            .iter()
            .filter_map(|f| f.limit)
            .max()
            .unwrap_or(self.fetch_limit)
            .min(self.fetch_limit);
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
        let mut stats = SourceStats {
            fetched: report.events.len(),
            candidate_limit_reached: report.events.len() >= candidate_limit,
            ..Default::default()
        };
        let mut accepted = Vec::new();
        for row in report.events.drain(..) {
            if matches!(
                self.policy
                    .check_event(EventPolicyContext {
                        event: &row.event,
                        source: &row.source
                    })
                    .await?,
                PolicyDecision::Drop { .. }
            ) {
                stats.graph_rejected += 1;
            } else if !discovery::matches_text(row.event.as_event(), &self.contains) {
                stats.text_rejected += 1;
            } else {
                accepted.push(row);
            }
        }
        stats.matched = accepted.len();
        *self.stats.lock().unwrap() = Some(stats);
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
    query_inner(request, None).await
}

/// Add caller-supplied events as an in-memory source. No temporary event files.
/// Verifies every event, including when graph policy would exclude its author.
pub async fn query_with_events(
    request: Request,
    events: Vec<nostr::Event>,
) -> Result<serde_json::Value> {
    let events = events
        .into_iter()
        .map(VerifiedEvent::try_from)
        .collect::<nostr_pubsub::Result<Vec<_>>>()?;
    query_inner(request, Some(events)).await
}

async fn query_inner(
    mut request: Request,
    input: Option<Vec<VerifiedEvent>>,
) -> Result<serde_json::Value> {
    ensure!(
        request.contains.iter().all(|text| !text.trim().is_empty()),
        "contains terms must not be empty"
    );
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
    if !request.sources.no_social_filter {
        request
            .sources
            .identity
            .get_or_insert_with(|| snapshot::DEFAULT_IDENTITY.into());
        if request.sources.graph.is_none() {
            request.sources.graph = Some(snapshot::cache_path()?);
        }
        request
            .sources
            .max_distance
            .get_or_insert(DEFAULT_MAX_DISTANCE);
    }
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
            || sources.cache.is_some()
            || input.is_some()
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
        sources.no_social_filter || sources.max_distance.is_none() || sources.graph.is_some(),
        "max-distance requires a graph snapshot"
    );
    let mut filter = request.filter.clone();
    let mut graph_state = None;
    let mut graph_author_count = None;
    let mut graph_filter_location = None;
    let policy: Arc<dyn PubsubPolicy> = if !sources.no_social_filter
        && let Some(path) = &sources.graph
    {
        let identity = sources
            .identity
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("graph requires a public identity"))?;
        let graph = graph::load_graph(identity, path).map_err(|error| anyhow::anyhow!(
            "cannot load social graph: {error}; run `iris-nostr graph fetch` or explicitly use --no-social-filter"))?;
        let eligible = graph.users_in_distance_order(sources.max_distance);
        let authors = eligible
            .iter()
            .filter_map(|author| graph::public_key(author).ok())
            .filter(|author| {
                filter
                    .authors
                    .as_ref()
                    .is_none_or(|specified| specified.contains(author))
            })
            .collect::<std::collections::BTreeSet<_>>();
        graph_author_count = Some(authors.len());
        // An empty authors filter may be widened by relays. Return without networking.
        if authors.is_empty() {
            return Ok(
                serde_json::json!({"schema_version":1,"events":[],"sources":[],"partial":false,
                "social_filter":true,"identity":graph.get_root(),"max_distance":sources.max_distance,
                "graph_author_count":0,"filter":request.filter,
                "text_filter":request.contains,
                "coverage":"no authors match the selected graph snapshot"}),
            );
        }
        graph_filter_location = Some(if authors.len() <= MAX_PUSHED_GRAPH_AUTHORS {
            filter.authors = Some(authors);
            "source-and-local"
        } else {
            "local"
        });
        let graph = Arc::new(RwLock::new(graph));
        graph_state = Some(graph.clone());
        Arc::new(SocialGraphPolicy::new(
            graph,
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
    if let Some(path) = &sources.cache {
        let (_, bus) = cache::reader(path, "cache")?;
        buses.push((
            SourceRoute::local_index("cache").with_dataset("cache")?,
            Arc::new(bus),
        ));
    }
    if let Some(events) = input {
        let id = "input";
        let bus = Arc::new(InMemoryEventBus::new());
        for event in events {
            bus.publish(event, EventSource::local_index(id)).await?;
        }
        buses.push((SourceRoute::local_index(id).with_dataset(id)?, bus));
    }
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
    let contains = Arc::new(request.contains.iter().map(|s| s.to_lowercase()).collect());
    let mut source_stats = std::collections::HashMap::new();
    for (route, bus) in buses {
        let stats = Arc::new(Mutex::new(None));
        source_stats.insert(route.id.clone(), stats.clone());
        router = router.with_query_source(RouterQuerySource::new(
            route,
            Arc::new(CheckedSource {
                bus,
                policy: policy.clone(),
                timeout: request.timeout,
                fetch_limit: request.fetch_limit,
                contains: Arc::clone(&contains),
                stats,
            }),
        ));
    }
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
            let follow_distance = graph_state.as_ref().map(|graph| {
                graph
                    .read()
                    .unwrap()
                    .get_follow_distance(&row.event.as_event().pubkey.to_hex())
            });
            serde_json::json!({
                "event": row.event.as_event(),
                "follow_distance": follow_distance,
                "sources": row.provenance.iter().map(|p| p.route_id.clone()).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let attempts = report.attempts.iter().map(|attempt| {
        let mut result = match &attempt.outcome {
            RouteAttemptOutcome::Success { event_count } => serde_json::json!({"source": attempt.route.id, "ok": true, "event_count": event_count}),
            RouteAttemptOutcome::Failure { message } => serde_json::json!({"source": attempt.route.id, "ok": false, "error": message}),
        };
        if let Some(index) = attempt.route.id.strip_prefix("relay-").and_then(|s| s.parse::<usize>().ok()) {
            result["relay"] = serde_json::to_value(&relay_urls[index])?;
        }
        result["counts"] = serde_json::to_value(&*source_stats[&attempt.route.id].lock().unwrap())?;
        Ok::<_, anyhow::Error>(result)
    }).collect::<Result<Vec<_>>>()?;
    Ok(serde_json::json!({
        "schema_version": 1, "events": events, "sources": attempts,
        "social_filter": !sources.no_social_filter, "max_distance": sources.max_distance,
        "graph_author_count":graph_author_count,"graph_filter_location":graph_filter_location,
        "filter":request.filter,"text_filter":request.contains,
        "partial": !report.complete,
        "coverage": "bounded; source success does not prove complete Nostr history",
        "fetch_limit_per_source": request.fetch_limit,
        "identity": sources.identity.as_deref().map(graph::public_key).transpose()?.map(|key| key.to_hex()),
    }))
}
