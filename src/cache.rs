//! Portable Hashtree event snapshots. Writers serialize one pointer; immutable
//! blobs remain readable by other processes and are never garbage-collected here.
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use hashtree_core::{Hash, Store, StoreError, sha256};
use hashtree_fs::FsBlobStore;
use hashtree_nostr::{
    NostrEventStore, retain_unique_latest_events, stored_event_from_nostr_sdk_event,
};
use hashtree_nostr_pubsub::HashtreeNostrIndexEventBus;
use nostr::{Event, Filter};
use nostr_pubsub::{EventBus, EventSource, QueryOptions};
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Write, path::Path, sync::Arc};

pub const MAX_EVENTS: usize = 10_000;
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub schema_version: u32,
    pub root: Option<String>,
    pub event_count: usize,
    pub event_bytes: u64,
    pub max_events: usize,
    pub max_bytes: u64,
    pub synced_at: u64,
    pub oldest_event_at: Option<u64>,
    pub newest_event_at: Option<u64>,
    pub last_query: serde_json::Value,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).context("commit snapshot")?;
    Ok(())
}

pub fn state(path: &Path) -> Result<State> {
    let state: State = serde_json::from_slice(&crate::io::read_bounded(&path.join("state.json"))?)?;
    ensure!(state.schema_version == 1, "unsupported event cache schema");
    ensure!(
        (1..=MAX_EVENTS).contains(&state.max_events) && state.event_count <= state.max_events,
        "invalid event cache count"
    );
    ensure!(
        (1..=MAX_BYTES).contains(&state.max_bytes) && state.event_bytes <= state.max_bytes,
        "invalid event cache byte limit"
    );
    ensure!(
        (state.event_count == 0) == state.root.is_none(),
        "inconsistent event cache root"
    );
    Ok(state)
}

/// Filesystem block format shared with hashtree-fs / hashtree-client.
/// Fail on missing referenced blocks instead of returning an empty index result.
pub struct CacheStore(FsBlobStore);
impl CacheStore {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self(FsBlobStore::new(path)?))
    }
}
fn err(message: impl ToString) -> StoreError {
    StoreError::Other(message.to_string())
}
#[async_trait]
impl Store for CacheStore {
    async fn get(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        let bytes = self
            .0
            .get(hash)
            .await?
            .ok_or_else(|| err("required cache block is missing"))?;
        if bytes.len() > 8 * 1024 * 1024 || sha256(&bytes) != *hash {
            return Err(err("invalid cache block hash or size"));
        }
        Ok(Some(bytes))
    }
    async fn put(&self, hash: Hash, data: Vec<u8>) -> Result<bool, StoreError> {
        if data.len() > 8 * 1024 * 1024 || sha256(&data) != hash {
            return Err(err("invalid cache block hash or size"));
        }
        if self.0.has(&hash).await? {
            self.get(&hash).await?;
            return Ok(false);
        }
        self.0.put(hash, data).await
    }
    async fn has(&self, hash: &Hash) -> Result<bool, StoreError> {
        self.0.has(hash).await
    }
    async fn delete(&self, _: &Hash) -> Result<bool, StoreError> {
        Err(err("shared cache blobs are immutable"))
    }
}

pub fn reader(path: &Path, id: &str) -> Result<(State, HashtreeNostrIndexEventBus<CacheStore>)> {
    let state = state(path)?;
    ensure!(
        path.join("blobs").is_dir(),
        "event cache blobs directory is missing"
    );
    let root = state.root.as_deref().map(crate::parse_root).transpose()?;
    let bus = HashtreeNostrIndexEventBus::new(
        Arc::new(CacheStore::open(&path.join("blobs"))?),
        root,
        EventSource::local_index(id),
    );
    Ok((state, bus))
}

fn check_directory(path: &Path) -> Result<()> {
    if !path.exists() || path.join("state.json").exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(path)? {
        let name = entry?.file_name();
        ensure!(
            name == ".writer.lock" || name == "blobs",
            "choose a new event-cache directory, not another application's data directory"
        );
    }
    Ok(())
}

/// Reuse the protocol index's replaceable-event ordering before applying bounds.
/// An atomic checkpoint publishes the complete batch; no events are broadcast.
pub async fn merge(
    path: &Path,
    events: Vec<Event>,
    max_events: usize,
    max_bytes: u64,
    query: serde_json::Value,
) -> Result<State> {
    merge_inner(path, events, Some((max_events, max_bytes)), query).await
}

/// Preserve the destination's retention policy under the same writer lock.
/// A new cache starts with 5,000 events and 32 MiB of event data.
pub async fn merge_preserving_limits(
    path: &Path,
    events: Vec<Event>,
    query: serde_json::Value,
) -> Result<State> {
    merge_inner(path, events, None, query).await
}

async fn merge_inner(
    path: &Path,
    events: Vec<Event>,
    limits: Option<(usize, u64)>,
    query: serde_json::Value,
) -> Result<State> {
    let (max_events, max_bytes) = limits.unwrap_or((5000, 33_554_432));
    ensure!(
        (1..=MAX_EVENTS).contains(&max_events),
        "cache max-events must be 1..10000"
    );
    ensure!(
        (1..=MAX_BYTES).contains(&max_bytes),
        "cache max-bytes must be 1..134217728"
    );
    for event in &events {
        event.verify().context("invalid cache input event")?;
    }
    check_directory(path)?;
    std::fs::create_dir_all(path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.join(".writer.lock"))?;
    lock.try_lock()
        .context("event cache is being updated; retry after its writer finishes")?;
    let exists = path.join("state.json").exists();
    check_directory(path)?;
    let previous = if exists {
        Some(reader(path, "previous-cache")?)
    } else {
        None
    };
    let (max_events, max_bytes) = limits.unwrap_or_else(|| {
        previous
            .as_ref()
            .map_or((max_events, max_bytes), |(old, _)| {
                (old.max_events, old.max_bytes)
            })
    });
    let mut all = events
        .into_iter()
        .map(|e| stored_event_from_nostr_sdk_event(&e))
        .collect::<Vec<_>>();
    if let Some((old, bus)) = previous {
        let report = bus
            .query(
                vec![Filter::new()],
                QueryOptions {
                    limit: Some(old.max_events),
                },
            )
            .await?;
        ensure!(
            report.events.len() == old.event_count,
            "event cache count does not match its index"
        );
        all.extend(
            report
                .events
                .iter()
                .map(|row| stored_event_from_nostr_sdk_event(row.event.as_event())),
        );
    }
    let mut all = retain_unique_latest_events(all);
    all.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut bytes = 0;
    let mut retained = Vec::new();
    for event in all {
        let size = serde_json::to_vec(&event)?.len() as u64;
        if retained.len() < max_events && bytes + size <= max_bytes {
            bytes += size;
            retained.push(event);
        }
    }
    let store = Arc::new(CacheStore::open(&path.join("blobs"))?);
    let root = NostrEventStore::new(store)
        .build(None, retained.clone())
        .await?;
    let state = State {
        schema_version: 1,
        root: root.map(|cid| cid.to_string()),
        event_count: retained.len(),
        event_bytes: bytes,
        max_events,
        max_bytes,
        synced_at: now(),
        oldest_event_at: retained.iter().map(|e| e.created_at).min(),
        newest_event_at: retained.iter().map(|e| e.created_at).max(),
        last_query: query,
    };
    atomic_write(&path.join("state.json"), &serde_json::to_vec(&state)?)?;
    drop(lock);
    Ok(state)
}

pub fn status(path: &Path) -> Result<serde_json::Value> {
    let state = state(path)?;
    Ok(
        serde_json::json!({"cache":path,"state":state,"age_since_sync_seconds":now().saturating_sub(state.synced_at),"coverage":"bounded observed events; sync time is not proof of complete history","storage":"hashtree-fs blobs and a hashtree-nostr event-index root; obsolete blobs retained for other readers"}),
    )
}
