# Release Iris Nostr

The public name and executable are `iris-nostr`. Iris Chat owns `iris`.
Messaging and market operations are outside this package.

1. Review `git status`, staged paths and **all reachable history** for secrets,
   local paths, private endpoints, downloaded posts and configuration. Keep the
   explicit Cargo package include list. `.gitignore` is not a secret scanner.
2. Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
   `cargo test --locked`, and `cargo package --locked`. Review the packaged file
   list. Preserve complete logs locally outside tracked source.
3. Test a live graph-filtered query and an offline replay. Public graph data
   used for the default download must match the digest in `src/snapshot.rs`.
   Publish that explicitly selected public snapshot as `social-graph.bin` on
   the versioned GitHub release; never package a private user graph.
4. Commit and tag the exact release source. Push GitHub and Hashtree. For
   Hashtree use a qualified `git-remote-htree` and verify full source history in
   an empty-store public clone before publishing binaries.
5. Build the pinned revision using the reviewed `haps.toml` in Haps isolation.
   Toolchain: Rust 1.99.0 and host C compiler/SDK, preinstalled. Cargo's lockfile
   checks registry input checksums. No build helper downloads or scripts run.
   The initial binary target is `aarch64-apple-darwin`. Retain exact recipe,
   target, toolchain, payload bytes and executable permissions in local evidence.
   Never label source review or a normal build as a reproduced binary audit.
6. Stage only the executable in the target archive. Generate `release.json`
   with the full commit, version tag, asset sizes and SHA-256 hashes. Use the
   established Haps publisher and the mappings in `haps-release.json`; do not
   create a new publication identity. Run `haps import-release --check`, then
   publish and verify source, package availability and relay acknowledgements.
   Haps audit requirements remain in effect; do not invent a passing audit or
   weaken an installer's trust threshold to make a release appear complete.
7. Publish the verified Cargo archive. Verify registry checksum and packaged
   source after upload. Publish the plugin ZIP from the same committed source;
   validate both the source folder and extracted archive. Repository/plugin
   installation does not imply approval in a vendor's public directory.

No release command should add credentials, machine paths, cached research or
private operational metadata to the Git tree or public release assets.

For isolated Haps builds, put the dedicated build home outside the login-home
directory (for example a private temporary directory). Cargo walks parent
directories for configuration; a checkout beneath the login home may try to read
private host Cargo configuration, which isolation correctly denies. Keep signer
material outside source and staged packages. Do not switch to a host build.

The release recipe uses `scripts/build-release.sh` to remap checkout and Cargo
registry paths in distributed executables. Do not substitute a different binary
after verification; archive and publish the exact isolated-build payload.
