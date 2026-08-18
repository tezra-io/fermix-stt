#!/usr/bin/env bash
# Fetch the Silero VAD model that the `sherpa` build embeds in the binary.
#
# The default (hermetic) build does not need this. Run it once before
# `cargo build --features sherpa`; the release workflow runs it too.
set -euo pipefail

URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx"
SHA256="9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${repo_root}/assets/silero_vad.onnx"
mkdir -p "$(dirname "$dest")"

echo "fetching ${URL}"
curl --fail --location --silent --show-error --output "${dest}.tmp" "$URL"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "${dest}.tmp" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "${dest}.tmp" | cut -d' ' -f1)"
fi

if [ "$actual" != "$SHA256" ]; then
  rm -f "${dest}.tmp"
  echo "checksum mismatch for silero_vad.onnx" >&2
  echo "  expected ${SHA256}" >&2
  echo "  actual   ${actual}" >&2
  exit 1
fi

mv "${dest}.tmp" "$dest"
echo "wrote ${dest} (sha256 ${SHA256})"
