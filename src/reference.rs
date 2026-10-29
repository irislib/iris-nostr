//! Public NIP-19 / NIP-21 references. Relay hints are data, never connections.
use anyhow::{Result, bail, ensure};
use nostr::{
    EventId, PublicKey,
    nips::nip19::{FromBech32, Nip19},
};
use serde_json::{Value, json};

fn parse(value: &str) -> Result<Nip19> {
    let value = value.strip_prefix("nostr:").unwrap_or(value);
    ensure!(value.len() <= 16_384, "public reference is too long");
    ensure!(
        ["npub1", "nprofile1", "note1", "nevent1", "naddr1"]
            .iter()
            .any(|prefix| value.starts_with(prefix)),
        "expected a public npub, nprofile, note, nevent or naddr reference"
    );
    // Do not include the supplied value or library parser details in errors.
    Nip19::from_bech32(value).map_err(|_| anyhow::anyhow!("invalid public Nostr reference"))
}

pub fn public_key(value: &str) -> Result<PublicKey> {
    if let Ok(key) = PublicKey::from_hex(value) {
        return Ok(key);
    }
    match parse(value)? {
        Nip19::Pubkey(key) => Ok(key),
        Nip19::Profile(profile) => Ok(profile.public_key),
        _ => bail!("expected a public hex key, npub or nprofile"),
    }
}

pub fn event_id(value: &str) -> Result<EventId> {
    if let Ok(id) = EventId::from_hex(value) {
        return Ok(id);
    }
    match parse(value)? {
        Nip19::EventId(id) => Ok(id),
        Nip19::Event(event) => Ok(event.event_id),
        _ => bail!("expected a hex event ID, note or nevent"),
    }
}

/// Decode only public references. Hex is ambiguous; use `identity` or query `--id`.
pub fn decode(value: &str) -> Result<Value> {
    Ok(match parse(value)? {
        Nip19::Pubkey(key) => json!({"type":"npub", "pubkey":key.to_hex()}),
        Nip19::Profile(profile) => {
            json!({"type":"nprofile", "pubkey":profile.public_key.to_hex(), "relays":profile.relays})
        }
        Nip19::EventId(id) => json!({"type":"note", "id":id.to_hex()}),
        Nip19::Event(event) => {
            json!({"type":"nevent", "id":event.event_id.to_hex(), "pubkey":event.author.map(|key| key.to_hex()), "kind":event.kind.map(|kind| kind.as_u16()), "relays":event.relays})
        }
        Nip19::Coordinate(addr) => {
            json!({"type":"naddr", "pubkey":addr.public_key.to_hex(), "kind":addr.kind.as_u16(), "identifier":addr.identifier, "relays":addr.relays})
        }
        _ => bail!("only public Nostr references are supported"),
    })
}
