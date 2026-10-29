Keep this repository separate from Iris Chat. The executable is iris-nostr;
iris is already owned by Iris Chat. Messaging, linked-device authorization and ratchet state
remain owned by Iris Chat. General event signing uses an explicitly selected
existing Hashtree-format identity; never select a signer from the discovery root
or create/copy keys. Keep secrets out of command arguments and output. Use shared nostr-pubsub, Hashtree and graph libraries.
Do not open another application's live database or invent a second FIPS pool
when its explicit daemon interface can be used. Offline must perform no network
I/O, including through a local daemon. Keep library APIs free of CLI types.
Use locked builds and test with synthetic identities and local fixtures.

Default public graph entry point: Sirius Business Ltd; graph filtering defaults to
three hops, with explicit --no-social-filter opt-out. Never silently widen a query
when the graph is missing. Model inference belongs to the calling agent.
Keep private data out of commits and Cargo/plugin archives. See RELEASING.md.

Keep nak as a separate protocol toolbox. Add only small discovery/inspection
utilities backed by existing Rust primitives; compose tools through signed-event
JSONL. Do not add a nak runtime dependency or broad command-compatibility layer.

Prefer opt-in public Sources profiles and the shared hashtree-fs/hashtree-nostr
formats for cross-app reuse. Cache merges serialize writers and atomically
replace state.json; never remove old blobs while other readers may use them.
Profiles are kind-0 events in that same cache. Keep continuous graph crawling
with the existing Hashtree service; refresh is explicit, bootstrap is pinned.
Default graph entry points must have covered bootstrap data. Do not replace
a sibling's constant without checking its cold-start graph coverage.

The README is the canonical discovery guide for humans and agents. Keep the
plugin skill concise. CLI relay defaults apply only without selected sources;
never add networking to a local or offline read. Preserve explicit Nostr
filters when enforcing large graphs locally. Count tag popularity by distinct
full public keys over verified samples, not raw post volume or key prefixes.

Publishing requires matching relay OK evidence; queueing is not delivery.
Validate the whole batch before networking. Use synthetic identities and local
relays for write tests; a discovery request never authorizes a public post.
