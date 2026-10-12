<p align="center">
  <img src="assets/gray-logo.svg" alt="gray" width="96">
</p>
<h1 align="center">gray-opencode-sub</h1>
<p align="center">Run gray turns on OpenCode Zen and OpenCode Go — API-key provider sidecar.</p>

Forwards gray's Responses request to OpenCode's hosted Zen and Go endpoints with your
OpenCode API key. This is HTTP, not ACP: t3code does not drive OpenCode over ACP either.

## Providers

- `opencode-zen` — OpenCode Zen.
- `opencode-go` — OpenCode Go.

Both are API-key providers. The model list is fetched from each endpoint's `/models`.

## Authentication

The only key sources are the ones you set up for this plugin:

1. `OPENCODE_API_KEY` environment variable.
2. The sidecar's own cache, written from the key the host collects when you enter it in the
   provider login.

Get your key from https://opencode.ai/auth (create one under OpenCode Zen). Zen and Go take
the same key. The plugin does not read OpenCode's own credential store (`auth.json`, or the
`OPENCODE_AUTH_*` variables), nor any other app's login.

## Setup

1. Configure a key (above).
2. Run `/opencode` to see which of Zen and Go have keys configured.

Keys are sent only as bearer auth to OpenCode; error bodies are redacted before they reach
gray.

## License

MIT — see [LICENSE](LICENSE).
