#!/usr/bin/env bash
# Fetch the sherpa-onnx static library archive that the `sherpa` build links,
# and refuse it unless it matches the sha256 pinned here for the target.
#
# sherpa-onnx-sys's build script downloads this archive itself, unchecked,
# unless SHERPA_ONNX_ARCHIVE_DIR names a folder holding it: then it copies the
# archive from there and fails when it is missing, never downloading. The
# release workflow runs this script, then builds with SHERPA_ONNX_ARCHIVE_DIR
# set to the same folder. A target dir that already holds an unpacked archive
# (target/sherpa-onnx-prebuilt) is reused without either, which is why the
# release builds in a fresh checkout.
#
# Usage: scripts/fetch-sherpa-onnx.sh <rust-target> <absolute-dest-dir>
set -euo pipefail

# Must equal the sherpa-onnx-sys version in Cargo.lock: its build script asks
# for the archive of its own version. Bump it and every pin below together.
VERSION="1.13.5"

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <rust-target> <absolute-dest-dir>" >&2
  exit 2
fi
target="$1"
dest_dir="$2"

# sherpa-onnx-sys resolves SHERPA_ONNX_ARCHIVE_DIR from its own crate folder.
case "$dest_dir" in
  /*) ;;
  *)
    echo "dest dir must be an absolute path: ${dest_dir}" >&2
    exit 2
    ;;
esac

# One pin per release target, for the static archive sherpa-onnx-sys links.
case "$target" in
  x86_64-unknown-linux-gnu)
    platform="linux-x64"
    SHA256="2ade8b7c62de66b9cf2e32bd7dbe077addaa4b18f422b49dc1bf3a1a0b1f762e"
    ;;
  aarch64-unknown-linux-gnu)
    platform="linux-aarch64"
    SHA256="f78af8260892f3060c8c0aba9ae93e4e4c1b16fe509238b88e3688889235e1b2"
    ;;
  aarch64-apple-darwin)
    platform="osx-arm64"
    SHA256="339c8fc19bb4b26e118c80792bbc4546eb263040fac36ef0cc027ec29c756b44"
    ;;
  *)
    echo "no pinned sherpa-onnx archive for target ${target}" >&2
    exit 1
    ;;
esac

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
locked="$(grep -A1 '^name = "sherpa-onnx-sys"$' "${repo_root}/Cargo.lock" \
  | sed -n 's/^version = "\(.*\)"$/\1/p')"
if [ "$locked" != "$VERSION" ]; then
  echo "Cargo.lock locks sherpa-onnx-sys ${locked:-(none)} but this script pins ${VERSION}" >&2
  exit 1
fi

archive="sherpa-onnx-v${VERSION}-${platform}-static-lib.tar.bz2"
URL="https://github.com/k2-fsa/sherpa-onnx/releases/download/v${VERSION}/${archive}"
dest="${dest_dir}/${archive}"
mkdir -p "$dest_dir"
# The folder holds the archive only when this run checked it.
rm -f "$dest"

echo "fetching ${URL}"
curl --fail --location --silent --show-error --output "${dest}.tmp" "$URL"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "${dest}.tmp" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "${dest}.tmp" | cut -d' ' -f1)"
fi

if [ "$actual" != "$SHA256" ]; then
  rm -f "${dest}.tmp"
  echo "checksum mismatch for ${archive}" >&2
  echo "  expected ${SHA256}" >&2
  echo "  actual   ${actual}" >&2
  exit 1
fi

mv "${dest}.tmp" "$dest"
echo "wrote ${dest} (sha256 ${SHA256})"
