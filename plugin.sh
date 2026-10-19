#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/opencode-sub.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/opencode-sub" ;;
    *) rel="x86_64-apple-darwin/opencode-sub" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/opencode-sub" ;;
    *) rel="x86_64-unknown-linux-musl/opencode-sub" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/opencode-sub.exe" "$here/target/release/opencode-sub" "$here/target/debug/opencode-sub.exe" "$here/target/debug/opencode-sub"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "opencode-sub binary not found in $here" >&2
exit 1
