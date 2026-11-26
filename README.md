<p align="center">
  <img src="assets/gray-logo.svg" alt="gray" width="96">
</p>
<h1 align="center">gray-opencode-sub</h1>
<p align="center">Run Gray turns on OpenCode Zen and Go through the native OpenCode 2 server.</p>

The sidecar discovers models from the running OpenCode server and streams native session
output through Gray's Responses relay. Gray's provider IDs are unchanged: `opencode-zen`
selects the native `opencode` provider, and `opencode-go` selects native `opencode-go`.
Model IDs remain unqualified within each Gray provider.

## Setup

1. Install the OpenCode 2 CLI (verified with 2.3.0). The sidecar finds `opencode` on `PATH`;
   set `OPENCODE_BIN` to use a specific executable instead.
2. Set `OPENCODE_API_KEY` or enter a key from <https://opencode.ai/auth> in Gray. Zen and Go
   use the same key. `/opencode status` checks whether a key is configured.
3. Select a Zen or Go model in Gray.

By default the sidecar starts a password-protected server on an ephemeral loopback port.
It uses a temporary OpenCode profile, not your OpenCode configuration, credentials, or
session database. The workspace defaults to the sidecar's working directory; set
`GRAY_OPENCODE_DIRECTORY` to choose a different one.

To use an already running server, set `GRAY_OPENCODE_SERVER_URL` to
`http://127.0.0.1:<port>` and `GRAY_OPENCODE_SERVER_PASSWORD` to its password. The external
server must have its own Zen/Go credential configured; the sidecar does not inject its
key into an external server. Public-only models are not exposed.

Matching turns in a Gray conversation share an OpenCode session while the sidecar and
server are running. Isolated Gray requests get their own sessions. Closing the Gray
stream interrupts the OpenCode turn. Gray's host tools are not bridged into OpenCode,
and native tool permissions are denied rather than granted without Gray approval.

## License

MIT — see [LICENSE](LICENSE).
