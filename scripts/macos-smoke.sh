#!/usr/bin/env bash
# Runs on an Apple silicon Mac against a running kumobad that has the macos image
# (kumobad macos-image). CI runners cannot boot macOS guests, so this is the local check
# for macOS sandboxes before a release.
#   scripts/macos-smoke.sh [cli command...]      default: .local/bin/kumoba
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $# -gt 0 ]]; then cli=("$@"); else cli=(.local/bin/kumoba); fi
scratch=$(mktemp -d "${TMPDIR:-/tmp}/kumoba-macos.XXXXXX")
ids=()
cleanup() {
  for id in "${ids[@]}"; do "${cli[@]}" delete "$id" >/dev/null 2>&1 || true; done
  rm -rf "$scratch"
}
trap cleanup EXIT
check() { if [[ "$2" != "$3" ]]; then printf 'FAIL %s: expected [%s], got [%s]\n' "$1" "$3" "$2" >&2; exit 1; fi; printf 'ok   macos %s\n' "$1"; }
status() { set +e; "$@" > "$scratch/out" 2> "$scratch/err"; local code=$?; set -e; echo "$code"; }
now() { python3 -c 'import time; print(time.time())'; }
seconds() { python3 -c "print(round($2 - $1, 1))"; }

id=$("${cli[@]}" create --image macos --network.mode allowlist network.hosts=example.com -q)
ids+=("$id")
began=$(now)
"${cli[@]}" start "$id" -q > /dev/null
printf 'time macos start %ss\n' "$(seconds "$began" "$(now)")"
check os "$("${cli[@]}" exec "$id" -- sw_vers -productName)" macOS
check user "$("${cli[@]}" exec "$id" --user kumoba -- sh -c 'whoami; sudo -n true && echo sudo')" "kumoba
sudo"
check developer-tools "$("${cli[@]}" exec "$id" --user kumoba -- sh -c 'git --version >/dev/null && python3 -c "print(6*7)"')" 42
check workspace "$("${cli[@]}" exec "$id" --user kumoba --cwd /workspace -- sh -c 'touch made && ls')" made
drift=$(( $("${cli[@]}" exec "$id" -- date -u +%s) - $(date -u +%s) ))
check clock "$(( drift < 2 && drift > -2 ))" 1
check exit-status "$(status "${cli[@]}" exec "$id" -- sh -c 'echo out; echo err >&2; exit 7')" 7
check streams "$(cat "$scratch/out")/$(cat "$scratch/err")" out/err
"${cli[@]}" exec "$id" -- sh -c 'head -c 104857600 /dev/urandom > /tmp/big; shasum -a 256 /tmp/big | cut -d" " -f1 > /tmp/big.sum; cat /tmp/big' > "$scratch/big"
check big-output "$(shasum -a 256 "$scratch/big" | cut -d' ' -f1)" "$("${cli[@]}" exec "$id" -- cat /tmp/big.sum)"
head -c 4194304 /dev/urandom > "$scratch/upload"
"${cli[@]}" write-file "$id" --path /tmp/upload < "$scratch/upload" > /dev/null
"${cli[@]}" read-file "$id" path=/tmp/upload > "$scratch/download"
check file-roundtrip "$(cmp -s "$scratch/upload" "$scratch/download" && echo same)" same
check allowed-host "$("${cli[@]}" exec "$id" -- curl -sS -o /dev/null -w '%{http_code}' https://example.com)" 200
check blocked-host "$(status "${cli[@]}" exec "$id" -- curl -sS -o /dev/null https://github.com)" 56
check shell-pipe "$(printf 'echo shell-ok\nexit 3\n' | "${cli[@]}" shell "$id" 2>&1 | grep -c '^shell-ok')" 1
check shell-status "$(set +e; printf 'exit 3\n' | "${cli[@]}" shell "$id" > /dev/null 2>&1; echo $?)" 3
python3 scripts/shell-pty-test.py "$id" "${cli[@]}"

second=$("${cli[@]}" create --image macos -q)
ids+=("$second")
began=$(now)
"${cli[@]}" start "$second" -q > /dev/null
printf 'time macos start beside another %ss\n' "$(seconds "$began" "$(now)")"
check two-at-once "$("${cli[@]}" exec "$second" -- sw_vers -productName)" macOS
third=$("${cli[@]}" create --image macos -q)
ids+=("$third")
check third-refused "$(status "${cli[@]}" start "$third" -q)" 1
check third-reason "$(grep -c 'capacity' "$scratch/err")" 1

began=$(now)
check stop "$(status "${cli[@]}" stop "$second" -q)" 0
printf 'time macos stop %ss\n' "$(seconds "$began" "$(now)")"
check clean-stop "$(grep -o '"clean":true' "${KUMOBA_HOME:-$HOME/.kumoba}/state/runtime/sandboxes/$second/manifest.json")" '"clean":true'
began=$(now)
"${cli[@]}" start "$second" -q > /dev/null
printf 'time macos restart %ss\n' "$(seconds "$began" "$(now)")"
check kept-disk "$("${cli[@]}" exec "$second" -- sh -c 'test -d /Users/kumoba && echo yes')" yes
for sandbox in "$id" "$second"; do "${cli[@]}" stop "$sandbox" -q > /dev/null; done
check run "$("${cli[@]}" run --image macos -- sw_vers -productName)" macOS
offline=$("${cli[@]}" create --image macos --network.mode blocked -q)
ids+=("$offline")
"${cli[@]}" start "$offline" -q > /dev/null
check network-blocked "$([[ "$(status "${cli[@]}" exec "$offline" -- curl -sS -m 10 -o /dev/null https://example.com)" != 0 ]] && echo offline)" offline
echo MACOS_SMOKE_PASS
