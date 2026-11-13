#!/usr/bin/env bash
# Installs kumobad, kumoba-worker and a guest image from a GitHub release into ~/.kumoba:
#   curl -fsSL https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh | bash
# Also run by scripts/release-smoke.sh against the release files before they are published.
# Settings, all optional:
#   KUMOBA_VERSION       release to install, e.g. 0.1.0 (default: the latest release)
#   KUMOBA_HOME          install directory (default ~/.kumoba)
#   KUMOBA_IMAGE         default, test or none (default: default, about a 450 MB download)
#   KUMOBA_INSTALL_FROM  directory holding the release files instead of downloading them
set -euo pipefail
repo=Parassharmaa/kumoba
home=${KUMOBA_HOME:-$HOME/.kumoba}
image=${KUMOBA_IMAGE:-default}
from=${KUMOBA_INSTALL_FROM:-}

say() { printf '%s\n' "$*" >&2; }
fail() { say "kumoba install: $*"; exit 1; }

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) platform=macos-arm64; arch=arm64 ;;
  Linux/x86_64) platform=linux-x86_64; arch=x86_64 ;;
  Linux/aarch64 | Linux/arm64) platform=linux-arm64; arch=arm64 ;;
  *) fail "unsupported host $(uname -s)/$(uname -m); kumoba runs on Apple silicon macOS and on Linux x86_64 or arm64" ;;
esac
case "$image" in default | test | none) ;; *) fail "KUMOBA_IMAGE must be default, test or none" ;; esac

if ! command -v zstd >/dev/null; then
  if [[ "$platform" == macos-* ]]; then fail 'zstd is missing: brew install zstd'; fi
  fail 'zstd is missing: install it with your package manager (apt install zstd, dnf install zstd)'
fi
if command -v sha256sum >/dev/null; then sha256() { sha256sum -c --quiet -; }
else sha256() { shasum -a 256 -c --quiet -; }
fi
[[ -n "$from" ]] || command -v curl >/dev/null || fail 'curl is missing'

version=${KUMOBA_VERSION:-}
if [[ -z "$version" ]]; then
  [[ -z "$from" ]] || fail 'set KUMOBA_VERSION together with KUMOBA_INSTALL_FROM'
  latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") \
    || fail 'cannot reach github.com to find the latest release'
  version=${latest##*/v}
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || fail "no published release found at https://github.com/$repo/releases"
fi
version=${version#v}

work=$(mktemp -d "${TMPDIR:-/tmp}/kumoba-install.XXXXXX")
trap 'rm -rf "$work"' EXIT
fetch() {
  if [[ -n "$from" ]]; then
    cp "$from/$1" "$work/$1" 2>/dev/null || fail "$1 is not in $from"
  else
    curl -fL --retry 2 --progress-bar "https://github.com/$repo/releases/download/v$version/$1" -o "$work/$1" \
      || fail "cannot download $1 from release v$version"
  fi
  grep -E " \*?$1\$" "$work/SHA256SUMS" | sed 's/ \*/  /' | (cd "$work" && sha256) || fail "checksum mismatch for $1"
}

if [[ -n "$from" ]]; then cp "$from/SHA256SUMS" "$work/SHA256SUMS" || fail "SHA256SUMS is not in $from"
else curl -fsSL "https://github.com/$repo/releases/download/v$version/SHA256SUMS" -o "$work/SHA256SUMS" || fail "release v$version not found"
fi

say "Installing kumoba $version for $platform into $home"
tarball="kumoba-$version-$platform.tar.zst"
fetch "$tarball"
mkdir -p "$home/images"
rm -rf "${home:?}/bin" "${home:?}/lib"
zstd -dc "$work/$tarball" | tar -xf - -C "$home"

if [[ "$image" != none ]]; then
  marker="$home/images/$image.raw.version"
  if [[ -f "$home/images/$image.raw" && "$(cat "$marker" 2>/dev/null)" == "$version" ]]; then
    say "Image $image $version is already installed"
  else
    asset="kumoba-image-$image-$version-$arch.raw.zst"
    say "Downloading the $image image"
    fetch "$asset"
    partial="$home/images/$image.raw.next"
    trap 'rm -rf "$work" "$partial"' EXIT
    if ! zstd -d -q -f --sparse "$work/$asset" -o "$partial"; then
      free=$(df -Pk "$home/images" | awk 'NR == 2 { printf "%.1f GB", $4 / 1048576 }')
      fail "could not unpack the $image image into $home/images ($free free). It needs about 2 GB for default and a few MB for test; free some space and run the installer again"
    fi
    mv "$home/images/$image.raw.next" "$home/images/$image.raw"
    printf '%s\n' "$version" > "$marker"
  fi
fi

say ""
say "kumoba $version is installed in $home"
if [[ "$platform" == macos-* && ! -f /opt/homebrew/opt/libkrun/lib/libkrun.1.dylib ]]; then
  say ""
  say "libkrun is missing. Install it with Homebrew first:"
  say "  brew tap slp/krun && brew install slp/krun/libkrun slp/krun/libkrunfw"
fi
if [[ "$platform" == linux-* && ! -w /dev/kvm ]]; then
  say ""
  say "/dev/kvm is not writable by $(id -un). Add yourself to its group and log in again:"
  say "  sudo usermod -aG kvm $(id -un)"
fi
say ""
case ":$PATH:" in
  *":$home/bin:"*) ;;
  *)
    say "Add kumoba to your PATH, in your shell profile too:"
    say "  export PATH=\"$home/bin:\$PATH\""
    say ""
    ;;
esac
say "Start the daemon, then try it from another terminal:"
say "  kumobad"
say "  kumoba run -- python3 -c 'print(21 * 2)'"
say "  kumoba          # every command"
say ""
say "From code: npm install kumoba, then new Kumoba().run(\"python3 --version\")"
