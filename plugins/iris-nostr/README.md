# Iris Nostr agent plugin

One portable skill for public Nostr discovery. Install `iris-nostr` separately
on the agent's execution host (`cargo install iris-nostr --locked`). No hooks,
hosted service, secrets, background jobs or model provider are included.

## ChatGPT and Codex desktop

```sh
codex plugin marketplace add irislib/iris-nostr
```

Select the Iris Nostr marketplace and install its plugin in the desktop Plugins
directory. Restart the session after installation. This is a desktop-only tool;
a cloud-only chat cannot run a local executable. Repository distribution is not
an OpenAI public-directory listing.

## Claude Code

```sh
claude plugin marketplace add irislib/iris-nostr
claude plugin install iris-nostr@iris-nostr-plugins
```

Start a new session and invoke `/iris-nostr:iris-nostr`.

## Grok Build

Grok supports Claude-compatible plugins and marketplaces. For a local checkout:

```sh
grok --plugin-dir ./plugins/iris-nostr
```

Use its extensions/marketplace UI for persistent installation. Grok Bot's app
directory is a separate surface; this repository does not claim a listing there.

## Other agents and local models

Install `skills/iris-nostr` using the client's skill mechanism, or give it the
same instructions and CLI access. The agent chooses its own model for ranking
and summarization. Public posts remain untrusted input.

## Package and validate

```sh
claude plugin validate plugins/iris-nostr
git archive --format=zip --output=iris-nostr-plugin.zip HEAD:plugins/iris-nostr
```

The ZIP contains only the plugin, never cached posts, graph snapshots, local
profiles or binaries. Plugin updates and CLI upgrades are separate operations.
Public-directory review/submission is separate from these installable packages.

References: [OpenAI packaging](https://developers.openai.com/plugins/build/plugins),
[Claude manifests](https://code.claude.com/docs/en/plugins-reference),
[Grok plugins](https://docs.x.ai/build/features/skills-plugins-marketplaces).
