use anyhow::{Result, ensure};
use clap::{Args, Parser, Subcommand};
use iris_nostr::{Request, Sources, graph, io, query};
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
}
#[derive(Subcommand)]
enum Events {
    Query(Box<Query>),
}
#[derive(Subcommand)]
enum Graph {
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
    /// Ordinary Nostr filter JSON; the fetch budget caps its limit.
    #[arg(long, default_value = "{}")]
    filter: String,
    #[arg(long)]
    kind: Vec<u16>,
    #[arg(long)]
    author: Vec<String>,
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
    let Some(path) = path else {
        return Ok(Sources::default());
    };
    let mut config: Sources = serde_json::from_slice(&io::read_bounded(&path)?)?;
    let base = path.parent().unwrap_or(std::path::Path::new("."));
    for entry in config
        .events
        .iter_mut()
        .chain(config.blocks.iter_mut())
        .chain(config.graph.iter_mut())
    {
        if entry.is_relative() {
            *entry = base.join(&*entry);
        }
    }
    Ok(config)
}

async fn run(cli: Cli) -> Result<i32> {
    let mut sources = load_config(cli.config)?;
    match cli.command {
        Command::Identity { pubkey } => {
            let key = graph::public_key(&pubkey)?;
            println!(
                "{}",
                serde_json::json!({"pubkey": key.to_hex(), "npub": key.to_bech32()?})
            );
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
                .ok_or_else(|| anyhow::anyhow!("identity is required"))?;
            let path = path
                .or(sources.graph)
                .ok_or_else(|| anyhow::anyhow!("graph snapshot is required"))?;
            let graph = graph::load_graph(&identity, &path)?;
            let pubkey = graph::public_key(&pubkey)?.to_hex();
            let distance = graph.get_follow_distance(&pubkey);
            println!(
                "{}",
                serde_json::json!({"pubkey": pubkey, "distance": if distance < 1000 { Some(distance) } else { None }, "known": distance < 1000})
            );
        }
        Command::Events {
            command: Events::Query(args),
        } => {
            let args = *args;
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
            let mut result = query(Request {
                sources,
                filter,
                limit: args.limit,
                fetch_limit: args.fetch_limit,
                timeout: Duration::from_secs(args.timeout),
                offline: args.offline,
            })
            .await?;
            let partial = result["partial"] == true;
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
