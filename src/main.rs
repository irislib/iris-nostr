use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use iris_nostr::{
    Request, Sources, cache, graph, io, query, query_with_events, reference, snapshot,
};
use nostr::{Filter, Kind, nips::nip19::ToBech32};
use std::{io::Write, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    name = "iris-nostr",
    version,
    about = "Local-first Nostr events and social graph queries. Messaging: use Iris Chat's iris command."
)]
struct Cli {
    /// Public source configuration. Relative paths resolve beside this file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Inspect the selected public source profile. Also honors IRIS_NOSTR_CONFIG.
    Config,
    /// Inspect a portable local event cache without networking.
    Cache {
        #[command(subcommand)]
        command: Cache,
    },
    /// Read or refresh public kind-0 metadata in the shared event cache.
    Profiles {
        #[command(subcommand)]
        command: Profiles,
    },
    /// Read signed events. JSON report by default; no publishing or signing.
    Events {
        #[command(subcommand)]
        command: Events,
    },
    /// Inspect a local social graph snapshot.
    Graph {
        #[command(subcommand)]
        command: Graph,
    },
    /// Normalize a public identity; never reads private keys or app databases.
    Identity { pubkey: String },
    /// Decode a public NIP-19 reference or nostr: URI. Never follows relay hints.
    Decode { reference: String },
}
#[derive(Subcommand)]
enum Cache {
    Status {
        #[arg(long)]
        cache: Option<PathBuf>,
    },
}
#[derive(Subcommand)]
enum Events {
    Query(Box<Query>),
    /// Fetch a bounded sample and merge it into a portable Hashtree event cache.
    Sync {
        #[command(flatten)]
        query: Box<Query>,
        #[arg(long, default_value_t = 5000)]
        max_events: usize,
        #[arg(long, default_value_t = 33_554_432)]
        max_bytes: u64,
    },
    /// Check event IDs and signatures offline. No graph or network needed.
    Verify {
        /// Signed JSON or JSONL file; use - for stdin (the default).
        #[arg(long, default_value = "-")]
        events: PathBuf,
        /// Output verified events as JSONL, with the verification report on stderr.
        #[arg(long)]
        jsonl: bool,
    },
}
#[derive(Subcommand)]
enum Profiles {
    Get {
        pubkey: String,
        #[command(flatten)]
        sources: ProfileSources,
    },
    Refresh {
        pubkey: String,
        #[command(flatten)]
        sources: ProfileSources,
    },
}
#[derive(Args)]
struct ProfileSources {
    #[arg(long)]
    cache: Option<PathBuf>,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(long)]
    daemon: Option<String>,
    #[arg(long)]
    graph: Option<PathBuf>,
    #[arg(long)]
    identity: Option<String>,
    #[arg(long)]
    no_social_filter: bool,
    #[arg(long)]
    offline: bool,
    #[arg(long, default_value_t = 10)]
    timeout: u64,
}
#[derive(Subcommand)]
enum Graph {
    /// Inspect snapshot identity, observed list timestamps and local file age.
    Status {
        #[arg(long)]
        graph: Option<PathBuf>,
        #[arg(long)]
        identity: Option<String>,
    },
    /// Refresh from an explicitly trusted snapshot service or existing htree daemon.
    Refresh {
        #[arg(long, conflicts_with = "daemon")]
        url: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        identity: Option<String>,
    },
    /// Download and validate a graph snapshot for offline queries.
    Fetch {
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        sha256: Option<String>,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        identity: Option<String>,
    },
    Distance {
        pubkey: String,
        #[arg(long)]
        identity: Option<String>,
        #[arg(long)]
        graph: Option<PathBuf>,
    },
}
#[derive(Args)]
struct Query {
    /// Portable Hashtree event cache. For sync, this is the destination.
    #[arg(long)]
    cache: Option<PathBuf>,
    /// Signed JSON or JSONL file. Use - once to read stdin.
    #[arg(long = "events")]
    events: Vec<PathBuf>,
    #[arg(long = "index")]
    indexes: Vec<String>,
    #[arg(long)]
    blocks: Option<PathBuf>,
    /// Existing htree daemon, e.g. http://127.0.0.1:8080. May use its peers.
    #[arg(long)]
    daemon: Option<String>,
    #[arg(long = "relay")]
    relays: Vec<String>,
    #[arg(long)]
    identity: Option<String>,
    /// Signed follow/mute JSON(L), or a trusted binary .bin graph snapshot.
    #[arg(long)]
    graph: Option<PathBuf>,
    #[arg(long)]
    max_distance: Option<u32>,
    /// Opt out of the default social graph filter (including mutes).
    #[arg(long)]
    no_social_filter: bool,
    /// Case-insensitive text match within fetched candidates (not global search).
    #[arg(long)]
    contains: Option<String>,
    /// Ordinary Nostr filter JSON; the fetch budget caps its limit.
    #[arg(long, default_value = "{}")]
    filter: String,
    #[arg(long)]
    kind: Vec<u16>,
    #[arg(long)]
    author: Vec<String>,
    /// Exact event ID as hex, note or nevent (also nostr: URIs). Repeat to match any.
    /// Relay hints are not followed; select sources explicitly.
    #[arg(long)]
    id: Vec<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    #[arg(long, default_value_t = 1000)]
    fetch_limit: usize,
    #[arg(long, default_value_t = 10)]
    timeout: u64,
    /// Strictly no networking, including local daemon access.
    #[arg(long)]
    offline: bool,
    /// Emit raw signed events, one per line, suitable for later --events input.
    /// The query report without event bodies is emitted on stderr.
    #[arg(long)]
    jsonl: bool,
}

fn load_config(path: Option<PathBuf>) -> Result<Sources> {
    let path = path.or_else(|| std::env::var_os("IRIS_NOSTR_CONFIG").map(PathBuf::from));
    let Some(path) = path else {
        return Ok(Sources::default());
    };
    Sources::from_file(&path)
}

async fn run(cli: Cli) -> Result<i32> {
    let mut sources = load_config(cli.config)?;
    match cli.command {
        Command::Config => {
            println!(
                "{}",
                serde_json::json!({"sources":sources,"default_identity":snapshot::DEFAULT_IDENTITY,"default_graph":snapshot::cache_path()?,"sharing":"public source profile only; signing identities remain in their owning application"})
            );
        }
        Command::Cache {
            command: Cache::Status { cache: path },
        } => {
            let path = path
                .or(sources.cache)
                .context("select --cache or configure cache in the source profile")?;
            println!("{}", cache::status(&path)?);
        }
        Command::Profiles { command } => {
            return profiles(sources, command).await;
        }
        Command::Graph {
            command:
                Graph::Status {
                    graph: path,
                    identity,
                },
        } => {
            let path = path
                .or(sources.graph)
                .map(Ok)
                .unwrap_or_else(snapshot::cache_path)?;
            let identity = identity
                .or(sources.identity)
                .unwrap_or_else(|| snapshot::DEFAULT_IDENTITY.into());
            println!("{}", graph::status(&identity, &path)?);
        }
        Command::Graph {
            command:
                Graph::Refresh {
                    url,
                    daemon,
                    sha256,
                    output,
                    identity,
                },
        } => {
            let url = if let Some(url) = url {
                url
            } else {
                let daemon=daemon.or(sources.daemon).context("choose --daemon or --url for a fresh snapshot; graph fetch restores the pinned bootstrap")?;
                iris_nostr::blocks::daemon_url(&daemon)?
                    .join("api/socialgraph/snapshot")?
                    .to_string()
            };
            let path = output
                .or(sources.graph)
                .map(Ok)
                .unwrap_or_else(snapshot::cache_path)?;
            let identity = identity
                .or(sources.identity)
                .unwrap_or_else(|| snapshot::DEFAULT_IDENTITY.into());
            let mut report =
                snapshot::fetch(Some(&url), sha256.as_deref(), &path, &identity).await?;
            report["refreshed_at"] = cache::now().into();
            println!("{report}");
        }
        Command::Decode { reference } => {
            println!("{}", reference::decode(&reference)?);
        }
        Command::Events {
            command: Events::Verify { events, jsonl },
        } => {
            let events = if events == std::path::Path::new("-") {
                io::read_events_from(std::io::stdin().lock())?
            } else {
                io::read_events(&events)?
            };
            let report = serde_json::json!({"schema_version":1, "valid":true, "event_count":events.len(), "checks":["event_id", "signature"]});
            if jsonl {
                let stdout = std::io::stdout();
                let mut output = stdout.lock();
                for event in events {
                    writeln!(output, "{}", serde_json::to_string(&event)?)?;
                }
                eprintln!("{report}");
            } else {
                println!("{report}");
            }
        }
        Command::Identity { pubkey } => {
            let key = graph::public_key(&pubkey)?;
            println!(
                "{}",
                serde_json::json!({"pubkey": key.to_hex(), "npub": key.to_bech32()?})
            );
        }
        Command::Graph {
            command:
                Graph::Fetch {
                    url,
                    sha256,
                    output,
                    identity,
                },
        } => {
            let path = output
                .or(sources.graph)
                .map(Ok)
                .unwrap_or_else(snapshot::cache_path)?;
            let identity = identity
                .or(sources.identity)
                .unwrap_or_else(|| snapshot::DEFAULT_IDENTITY.into());
            let result =
                snapshot::fetch(url.as_deref(), sha256.as_deref(), &path, &identity).await?;
            println!("{result}");
        }
        Command::Graph {
            command:
                Graph::Distance {
                    pubkey,
                    identity,
                    graph: path,
                },
        } => {
            let identity = identity
                .or(sources.identity)
                .unwrap_or_else(|| snapshot::DEFAULT_IDENTITY.into());
            let path = path
                .or(sources.graph)
                .map(Ok)
                .unwrap_or_else(snapshot::cache_path)?;
            let graph = graph::load_graph(&identity, &path)?;
            let pubkey = graph::public_key(&pubkey)?.to_hex();
            let distance = graph.get_follow_distance(&pubkey);
            println!(
                "{}",
                serde_json::json!({"pubkey": pubkey, "distance": if distance < 1000 { Some(distance) } else { None }, "known": distance < 1000})
            );
        }
        Command::Events {
            command: command @ (Events::Query(_) | Events::Sync { .. }),
        } => {
            let (args, sync) = match command {
                Events::Query(args) => (args, None),
                Events::Sync {
                    query,
                    max_events,
                    max_bytes,
                } => (query, Some((max_events, max_bytes))),
                _ => unreachable!(),
            };
            let args = *args;
            sources.cache = args.cache.or(sources.cache);
            let destination = if let Some((max_events, max_bytes)) = sync {
                ensure!(
                    (1..=cache::MAX_EVENTS).contains(&max_events)
                        && (1..=cache::MAX_BYTES).contains(&max_bytes),
                    "invalid cache retention limits"
                );
                Some(
                    sources
                        .cache
                        .take()
                        .context("sync requires --cache or a configured cache destination")?,
                )
            } else {
                None
            };
            if !args.events.is_empty() {
                sources.events = args.events;
            }
            if !args.indexes.is_empty() {
                sources.indexes = args.indexes;
            }
            if !args.relays.is_empty() {
                sources.relays = args.relays;
            }
            sources.blocks = args.blocks.or(sources.blocks);
            sources.daemon = args.daemon.or(sources.daemon);
            sources.identity = args.identity.or(sources.identity);
            sources.graph = args.graph.or(sources.graph);
            sources.max_distance = args.max_distance.or(sources.max_distance);
            sources.no_social_filter |= args.no_social_filter;
            let mut filter: Filter = serde_json::from_str(&args.filter)?;
            ensure!(
                filter.search.is_none() || sources.indexes.is_empty(),
                "Hashtree event indexes do not support NIP-50 search"
            );
            for kind in args.kind {
                filter = filter.kind(Kind::from(kind));
            }
            for author in args.author {
                filter = filter.author(graph::public_key(&author)?);
            }
            for id in args.id {
                filter = filter.id(reference::event_id(&id)?);
            }
            let stdin_count = sources
                .events
                .iter()
                .filter(|path| *path == std::path::Path::new("-"))
                .count();
            ensure!(stdin_count <= 1, "stdin may be selected only once");
            sources
                .events
                .retain(|path| path != std::path::Path::new("-"));
            let request_filter = serde_json::to_value(&filter)?;
            let request = Request {
                sources,
                filter,
                limit: args.limit,
                fetch_limit: args.fetch_limit,
                timeout: Duration::from_secs(args.timeout),
                offline: args.offline,
            };
            let mut result = if stdin_count == 1 {
                let events = io::read_events_from(std::io::stdin().lock())?;
                query_with_events(request, events).await?
            } else {
                query(request).await?
            };
            if let Some(text) = args.contains {
                let needle = text.to_lowercase();
                result["events"].as_array_mut().unwrap().retain(|row| {
                    row["event"]["content"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
                });
                result["text_filter"] =
                    serde_json::json!({"contains": text, "scope": "returned candidates only"});
            }
            let partial = result["partial"] == true;
            if let Some(destination) = destination {
                if partial {
                    result["cache_update"] = serde_json::json!({"updated":false,"reason":"source failure; previous cache preserved"});
                } else {
                    let events = result["events"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| serde_json::from_value(row["event"].clone()))
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    let (max_events, max_bytes) = sync.unwrap();
                    let mut summary = result.clone();
                    summary.as_object_mut().unwrap().remove("events");
                    summary["filter"] = request_filter;
                    let state =
                        cache::merge(&destination, events, max_events, max_bytes, summary).await?;
                    result["cache_update"] =
                        serde_json::json!({"updated":true,"path":destination,"state":state});
                }
            }
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            if args.jsonl {
                for event in result["events"].as_array().unwrap() {
                    writeln!(output, "{}", event["event"])?;
                }
                result.as_object_mut().unwrap().remove("events");
                eprintln!("{result}");
            } else {
                writeln!(output, "{result}")?;
            }
            return Ok(if partial { 3 } else { 0 });
        }
    }
    Ok(0)
}

async fn profiles(mut sources: Sources, command: Profiles) -> Result<i32> {
    let (pubkey, args, refresh) = match command {
        Profiles::Get { pubkey, sources } => (pubkey, sources, false),
        Profiles::Refresh { pubkey, sources } => (pubkey, sources, true),
    };
    let pubkey = graph::public_key(&pubkey)?;
    sources.cache = args.cache.or(sources.cache);
    sources.daemon = args.daemon.or(sources.daemon);
    sources.graph = args.graph.or(sources.graph);
    sources.identity = args.identity.or(sources.identity);
    sources.no_social_filter |= args.no_social_filter;
    if !args.relays.is_empty() {
        sources.relays = args.relays;
    }
    let destination = if refresh {
        Some(
            sources
                .cache
                .take()
                .context("profile refresh requires a cache destination")?,
        )
    } else {
        None
    };
    let request = |sources| Request {
        sources,
        filter: Filter::new().author(pubkey).kind(Kind::Metadata),
        limit: 1000,
        fetch_limit: 1000,
        timeout: Duration::from_secs(args.timeout),
        offline: args.offline,
    };
    let mut report = query(request(sources.clone())).await?;
    let partial = report["partial"] == true;
    if let Some(path) = destination {
        if partial {
            report["cache_update"] = serde_json::json!({"updated":false,"reason":"source failure; previous cache preserved"});
        } else {
            let events = report["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| serde_json::from_value(row["event"].clone()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut summary = report.clone();
            summary.as_object_mut().unwrap().remove("events");
            summary["filter"] = serde_json::json!({"kinds":[0],"authors":[pubkey.to_hex()]});
            let state = cache::merge_preserving_limits(&path, events, summary).await?;
            sources.cache = Some(path.clone());
            sources.events.clear();
            sources.indexes.clear();
            sources.blocks = None;
            sources.daemon = None;
            sources.relays.clear();
            report = query(request(sources)).await?;
            report["cache_update"] = serde_json::json!({"updated":true,"path":path,"state":state});
        }
    }
    let rows = report["events"].as_array_mut().unwrap();
    rows.sort_by(|a, b| {
        b["event"]["created_at"]
            .as_u64()
            .cmp(&a["event"]["created_at"].as_u64())
            .then_with(|| a["event"]["id"].as_str().cmp(&b["event"]["id"].as_str()))
    });
    let profile=rows.first().map(|row| {
        let event=&row["event"];
        let metadata=serde_json::from_str::<serde_json::Value>(event["content"].as_str().unwrap_or("")).ok().filter(|v|v.is_object());
        serde_json::json!({"event":event,"metadata":metadata,"metadata_valid":metadata.is_some(),"sources":row["sources"]})
    });
    report.as_object_mut().unwrap().remove("events");
    report["profile"] = serde_json::to_value(profile)?;
    report["pubkey"] = pubkey.to_hex().into();
    println!("{report}");
    Ok(if report["partial"] == true { 3 } else { 0 })
}

#[tokio::main]
async fn main() {
    match run(Cli::parse()).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{}", serde_json::json!({"error": format!("{error:#}")}));
            std::process::exit(1);
        }
    }
}
