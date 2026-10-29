Keep this repository separate from Iris Chat. The executable is iris-nostr;
iris is already owned by Iris Chat. Messaging, signing keys and ratchet state
remain owned by Iris Chat. Use shared nostr-pubsub, Hashtree and graph libraries.
Do not open another application's live database or invent a second FIPS pool
when its explicit daemon interface can be used. Offline must perform no network
I/O, including through a local daemon. Keep library APIs free of CLI types.
Use locked builds and test with synthetic identities and local fixtures.

Default public graph entry point: Sirius Business Ltd; graph filtering defaults to
two hops, with explicit --no-social-filter opt-out. Never silently widen a query
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
