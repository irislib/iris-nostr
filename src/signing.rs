//! Explicit access to the existing Hashtree key format. No key creation or writes.
use anyhow::{Context, Result, bail, ensure};
use nostr::{
    Event, EventBuilder, EventId, Keys, Kind, PublicKey, Tag, Timestamp, nips::nip19::ToBech32,
};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_DRAFT_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft {
    pub kind: u16,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<Vec<String>>,
    pub created_at: Option<u64>,
    pub pubkey: Option<PublicKey>,
    pub id: Option<EventId>,
}

#[derive(Serialize)]
pub struct PublicIdentity {
    pub alias: Option<String>,
    pub pubkey: String,
    pub npub: String,
    pub can_sign: bool,
}

struct Identity {
    public: PublicIdentity,
    keys: Option<Keys>,
}

struct KeyEntries(Vec<hashtree_config::KeyEntry>);
impl Drop for KeyEntries {
    fn drop(&mut self) {
        for entry in &mut self.0 {
            entry.secret.zeroize();
            if let Some(alias) = &mut entry.alias {
                alias.zeroize();
            }
        }
    }
}

fn load(path: &Path) -> Result<Vec<Identity>> {
    let mut bytes = Zeroizing::new(Vec::new());
    std::fs::File::open(path)
        .context("cannot open selected key file")?
        .take(65_537)
        .read_to_end(&mut bytes)
        .context("cannot read selected key file")?;
    ensure!(bytes.len() <= 65_536, "key file exceeds 64 KiB");
    let content = std::str::from_utf8(&bytes).context("key file is not UTF-8")?;
    let mut result = Vec::new();
    let mut entries = KeyEntries(hashtree_config::parse_keys_file(content));
    for (index, entry) in entries.0.iter_mut().enumerate() {
        ensure!(
            entry
                .alias
                .as_ref()
                .is_none_or(|alias| !alias.contains("nsec1")),
            "identity alias must not contain a secret key"
        );
        let parsed = if entry.secret.starts_with("nsec1") {
            Keys::parse(&entry.secret)
                .map(|keys| (keys.public_key(), Some(keys)))
                .map_err(|_| ())
        } else {
            PublicKey::parse(&entry.secret)
                .map(|public| (public, None))
                .map_err(|_| ())
        };
        entry.secret.zeroize();
        let (pubkey, keys) = parsed.map_err(|_| {
            anyhow::anyhow!(
                "invalid identity at entry {}; expected nsec, npub or public hex",
                index + 1
            )
        })?;
        result.push(Identity {
            public: PublicIdentity {
                alias: entry.alias.clone(),
                pubkey: pubkey.to_hex(),
                npub: pubkey.to_bech32()?,
                can_sign: keys.is_some(),
            },
            keys,
        });
    }
    Ok(result)
}

/// Lists only public identifiers. Public-only entries cannot sign.
pub fn identities(path: &Path) -> Result<Vec<PublicIdentity>> {
    Ok(load(path)?.into_iter().map(|entry| entry.public).collect())
}

pub fn read_draft(reader: impl Read) -> Result<Draft> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_DRAFT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_DRAFT_BYTES, "draft exceeds 1 MiB");
    serde_json::from_slice(&bytes).context("expected one unsigned event object with kind, content and optional tags, created_at, pubkey, id")
}

/// Signs without networking. The selected key must agree with any draft author.
pub fn sign(draft: Draft, keys: &Keys) -> Result<Event> {
    ensure!(
        draft.pubkey.is_none_or(|key| key == keys.public_key()),
        "draft author does not match selected signer"
    );
    let tags = draft
        .tags
        .into_iter()
        .map(Tag::parse)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let event = EventBuilder::new(Kind::from(draft.kind), draft.content)
        .tags(tags)
        .custom_created_at(
            draft
                .created_at
                .map(Timestamp::from)
                .unwrap_or_else(Timestamp::now),
        )
        .sign_with_keys(keys)?;
    ensure!(
        draft.id.is_none_or(|id| id == event.id),
        "draft ID does not match event fields"
    );
    ensure!(
        serde_json::to_vec(&event)?.len() <= MAX_DRAFT_BYTES,
        "signed event exceeds 1 MiB"
    );
    Ok(event)
}

/// Select by exact alias or public key; never fall back to the first key.
pub fn sign_from_store(draft: Draft, path: &Path, selector: &str) -> Result<Event> {
    ensure!(
        !selector.starts_with("nsec1"),
        "select a signer by alias or public key; never pass secret keys as arguments"
    );
    let public = crate::reference::public_key(selector)
        .ok()
        .map(|key| key.to_hex());
    let mut candidates = load(path)?.into_iter().filter(|entry| {
        entry.public.alias.as_deref() == Some(selector)
            || public.as_ref() == Some(&entry.public.pubkey)
    });
    let Some(selected) = candidates.next() else {
        bail!("signer not found; use identities to inspect the selected key file")
    };
    ensure!(
        candidates.next().is_none(),
        "signer selection is ambiguous; select an unambiguous public key or fix duplicate entries in the owning tool"
    );
    let keys = selected.keys.context(
        "selected identity has no signing key; use its owning signer and publish the signed event",
    )?;
    sign(draft, &keys)
}
