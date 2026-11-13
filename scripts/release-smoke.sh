#!/usr/bin/env bash
# Called by .github/workflows/release.yml on a KVM runner before anything is published: installs
# the release files with scripts/install.sh exactly as a user would, starts kumobad with no
# configuration and boots both images through the TypeScript SDK with no options.
#   scripts/release-smoke.sh <version> <assets-dir>
set -euo pipefail
cd "$(dirname "$0")/.."
version=${1:?usage: release-smoke.sh <version> <assets-dir>}
assets=$(cd "${2:?usage: release-smoke.sh <version> <assets-dir>}" && pwd)
[[ -e /dev/kvm ]] || { echo 'needs /dev/kvm' >&2; exit 2; }
sudo chmod 0666 /dev/kvm
(cd "$assets" && sha256sum kumoba-* > SHA256SUMS)
home=$(mktemp -d /tmp/kh.XXXXXX)
for image in default test; do
  KUMOBA_HOME="$home" KUMOBA_VERSION="$version" KUMOBA_IMAGE="$image" KUMOBA_INSTALL_FROM="$assets" bash scripts/install.sh
done
env -i HOME="$HOME" PATH="/usr/bin:/bin" KUMOBA_HOME="$home" "$home/bin/kumobad" 2> "$home/kumobad.log" &
daemon=$!
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null; rm -rf "$home"' EXIT
for _ in $(seq 100); do [[ -S "$home/kumobad.sock" ]] && break; sleep 0.1; done
[[ -S "$home/kumobad.sock" ]] || { cat "$home/kumobad.log" >&2; exit 1; }
cat "$home/kumobad.log"
cd sdk/typescript
npm ci --no-fund --no-audit
npm run build
KUMOBA_HOME="$home" node --input-type=module -e '
  import { Kumoba } from "./dist/index.js";
  const kumoba = new Kumoba();
  for (const image of ["default", "test"]) {
    await using sandbox = await kumoba.create({ image, user: "kumoba" });
    const result = await sandbox.run(image === "default" ? "python3 --version && node --version" : "uname -m");
    if (!result.ok) throw new Error(`${image}: ${result.stderr}`);
    console.log(`ok   ${image}: ${result.stdout.trim().replaceAll("\n", ", ")}`);
  }
'
echo RELEASE_SMOKE_PASS
