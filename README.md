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

Key resolution order:

1. `OPENCODE_API_KEY` environment variable.
2. OpenCode's own auth store — `OPENCODE_AUTH_CONTENT`, or `OPENCODE_AUTH_JSON`, or
   `~/.local/share/opencode/auth.json` — entries for `opencode` or `opencode-go`.
3. The sidecar's cache.

Easiest path: `opencode auth login`, select OpenCode Zen or Go, and paste the key from
https://opencode.ai/auth.

## Setup

1. Configure a key (above).
2. Run `/opencode` to see which of Zen and Go have keys configured.

Keys are sent only as bearer auth to OpenCode; error bodies are redacted before they reach
gray.

## License

MIT — see [LICENSE](LICENSE).
