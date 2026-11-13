#!/usr/bin/env bash
# Called by .github/workflows/walkthrough.yml on a fresh runner after a release, and by hand on
# a clean machine: follows the README's Getting started from the public installer and the npm
# registry, the way a new user would, and checks every step.
#   scripts/walkthrough.sh [version]      KUMOBA_HOME picks the install directory
set -euo pipefail
version=${1:-}
home=${KUMOBA_HOME:-$HOME/.kumoba}
export KUMOBA_HOME="$home"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/kumoba-walkthrough.XXXXXX")
daemon=""
id=""
cleanup() {
  [[ -n "$id" ]] && kumoba delete "$id" >/dev/null 2>&1
  [[ -n "$daemon" ]] && kill "$daemon" 2>/dev/null && wait "$daemon" 2>/dev/null
  rm -rf "$scratch"
}
trap cleanup EXIT
step() { printf '\n== %s\n' "$*"; }
run() { printf '$ %s\n' "$*"; "$@"; }
expect() { if [[ "$2" != *"$3"* ]]; then printf 'FAIL %s: expected to contain [%s], got [%s]\n' "$1" "$3" "$2" >&2; exit 1; fi; printf 'ok   %s\n' "$1"; }

step "1. Install"
installer=https://raw.githubusercontent.com/Parassharmaa/kumoba/main/scripts/install.sh
printf '$ curl -fsSL %s | bash\n' "$installer"
curl -fsSL "$installer" | KUMOBA_VERSION="$version" bash
export PATH="$home/bin:$PATH"
expect "kumobad installed" "$(command -v kumobad)" "$home/bin/kumobad"
expect "kumoba installed" "$(command -v kumoba)" "$home/bin/kumoba"
[[ -z "$version" ]] || expect "release version" "$(kumoba --version)" "kumoba $version"

step "2. Start the daemon"
printf '$ kumobad &\n'
kumobad 2> "$scratch/kumobad.log" &
daemon=$!
for _ in $(seq 100); do [[ -S "$home/kumobad.sock" ]] && break; sleep 0.1; done
cat "$scratch/kumobad.log"
expect "daemon ready" "$(cat "$scratch/kumobad.log")" '"event":"ready"'

step "3. Run something"
run kumoba | head -8
expect "one-shot run" "$(run kumoba run -- python3 -c 'import platform; print(platform.platform())')" "Linux"
id=$(run kumoba create --image default -q)
expect "create" "$id" "-"
run kumoba start "$id" -q
expect "pip install" "$(run kumoba exec "$id" -- pip install --user --break-system-packages requests 2>&1)" "requests"
echo 'import requests; print("requests", requests.__version__)' | run kumoba write-file "$id" --path /workspace/hi.py
expect "exec script" "$(run kumoba exec "$id" --cwd /workspace -- python3 hi.py)" "requests "
expect "exit status" "$(set +e; kumoba exec "$id" -- sh -c 'exit 7'; echo "status=$?")" "status=7"
view=$(run kumoba desktop "$id" --user kumoba --json)
url=$(printf '%s' "$view" | python3 -c 'import json,sys; print(json.load(sys.stdin)["url"])')
expect "desktop link" "$url" "/vnc.html?autoconnect=1"
page=$(curl -fsS -D - "$url")
expect "live view is a page" "$page" "text/html"
expect "live view is noVNC" "$page" "noVNC"
printf 'live view: %s\n' "$url"
if [[ -n "${KUMOBA_WALKTHROUGH_PAUSE:-}" ]]; then
  printf 'paused for %s s so the live view can be opened\n' "$KUMOBA_WALKTHROUGH_PAUSE"
  sleep "$KUMOBA_WALKTHROUGH_PAUSE"
fi
run kumoba delete "$id" -q
id=""

step "4. Use it from code"
mkdir -p "$scratch/app"
cd "$scratch/app"
run npm init -y > /dev/null
run npm install --no-fund --no-audit "kumoba${version:+@$version}" | tail -1
cat > main.mjs <<'EOF'
import { Kumoba } from "kumoba";

const kumoba = new Kumoba();

const once = await kumoba.run("python3 --version");
console.log(once.stdout.trim());

const sandbox = await kumoba.create({ user: "kumoba" });
await sandbox.files.write("/workspace/a.txt", "hello");
console.log((await sandbox.run("cat /workspace/a.txt")).stdout);
await sandbox.delete();
EOF
output=$(run node main.mjs)
printf '%s\n' "$output"
expect "sdk one-shot" "$output" "Python 3"
expect "sdk files and run" "$output" "hello"
expect "npx cli" "$(run npx --no-install kumoba run -- echo from-npx)" "from-npx"
expect "nothing left behind" "$(kumoba list --json)" '"sandboxes":[]'

printf '\nWALKTHROUGH_PASS\n'
