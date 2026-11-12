#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/host-env.sh
image=$(bash scripts/build-test-image.sh | tail -1)
work=$(mktemp -d "${TMPDIR:-/tmp}/kumobad-smoke.XXXXXX")
socket="$work/kumobad.sock"
head -c 48 /dev/urandom | base64 | tr -d '\n/+=' > "$work/token"
token=$(cat "$work/token")
KUMOBAD_STATE_DIR="$work/state" KUMOBAD_WORKER="$PWD/.local/bin/kumoba-worker" \
  KUMOBAD_IMAGES="test=$image" KUMOBAD_SOCKET="$socket" KUMOBAD_TOKEN_FILE="$work/token" \
  .local/bin/kumobad 2> "$work/daemon.log" &
daemon=$!
trap 'kill $daemon 2>/dev/null; wait $daemon 2>/dev/null; rm -rf "$work"' EXIT
for _ in $(seq 50); do [[ -S "$socket" ]] && break; sleep 0.1; done
api() { curl -sS --fail-with-body --unix-socket "$socket" -H "authorization: Bearer $token" "$@"; }
check() { if [[ "$2" != *"$3"* ]]; then printf 'FAIL %s: %s\n' "$1" "$2" >&2; cat "$work/daemon.log" >&2; exit 1; fi; printf 'ok   %s\n' "$1"; }

check unauthorized "$(curl -sS --unix-socket "$socket" http://d/v1/sandboxes)" unauthorized
created=$(api -X POST http://d/v1/sandboxes -H 'content-type: application/json' \
  -d '{"image":"test","labels":{"org":"o1","session":"s1"},"cpu":2,"memoryMib":512,"autoStopMinutes":15,"network":{"mode":"blocked"}}')
id=$(printf '%s' "$created" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
check create "$created" '"state":"stopped"'
check start "$(api -X POST "http://d/v1/sandboxes/$id/start")" '"state":"started"'
check list-filter "$(api "http://d/v1/sandboxes?label=org:o1&label=session:s1")" "$id"
check list-miss "$(api "http://d/v1/sandboxes?label=org:other")" '"sandboxes":[]'
out=$(api -X POST "http://d/v1/sandboxes/$id/exec" -H 'content-type: application/json' \
  -d '{"argv":["/bin/sh","-c","echo hello; echo oops >&2; exit 3"],"timeoutMs":10000,"user":"kumoba","cwd":"/home/kumoba"}')
check exec-stdout "$out" "$(printf 'hello\n' | base64)"
check exec-exit "$out" '"exitCode":3'
head -c 2500000 /dev/urandom > "$work/blob"
check upload "$(api -X PUT --data-binary @"$work/blob" "http://d/v1/sandboxes/$id/files?path=/home/kumoba/up/blob&user=kumoba")" '"written":2500000'
api -o "$work/back" "http://d/v1/sandboxes/$id/files?path=/home/kumoba/up/blob"
cmp "$work/blob" "$work/back" && echo "ok   download-matches"
check list-files "$(api "http://d/v1/sandboxes/$id/files/list?path=/home/kumoba/up")" '"name":"blob"'
api -m 2 -X POST "http://d/v1/sandboxes/$id/exec" -H 'content-type: application/json' \
  -d '{"argv":["/bin/sh","-c","echo $$ > /tmp/abort.pid; sleep 60"],"timeoutMs":60000}' >/dev/null 2>&1 || true
sleep 1
check abort-kills "$(api -X POST "http://d/v1/sandboxes/$id/exec" -H 'content-type: application/json' \
  -d '{"argv":["/bin/sh","-c","kill -0 $(cat /tmp/abort.pid) 2>/dev/null && echo alive || echo gone"],"timeoutMs":10000}')" "$(printf 'gone\n' | base64)"
check timeout "$(api -X POST "http://d/v1/sandboxes/$id/exec" -H 'content-type: application/json' \
  -d '{"argv":["/bin/sleep","30"],"timeoutMs":1000}')" '"timedOut":true'
api -X POST "http://d/v1/sandboxes/$id/exec" -H 'content-type: application/json' \
  -d '{"argv":["/bin/sh","-c","echo preview-ok > /workspace/index.html && setsid /usr/local/bin/http-fixture >/dev/null 2>&1 & while [ ! -f /workspace/http-ready ]; do sleep 0.1; done"],"timeoutMs":10000}' >/dev/null
preview_path=$(api -X POST "http://d/v1/sandboxes/$id/preview" -H 'content-type: application/json' -d '{"port":8080,"ttlSeconds":60}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["path"])')
check preview-tunnel "$(curl -sS --unix-socket "$socket" "http://d${preview_path}index.html")" preview-ok
check preview-forged "$(curl -sS --unix-socket "$socket" "http://d/v1/preview/AAAA.BBBB/index.html")" invalid
check access-log-empty "$(api "http://d/v1/access-log?sinceMs=0&untilMs=9999999999999")" '"entries":[]'
check stop "$(api -X POST "http://d/v1/sandboxes/$id/stop")" '"state":"stopped"'
check delete "$(api -X DELETE "http://d/v1/sandboxes/$id")" '"deleted":true'
check gone "$(curl -sS --unix-socket "$socket" -H "authorization: Bearer $token" "http://d/v1/sandboxes/$id")" not_found
open_id=$(api -X POST http://d/v1/sandboxes -H 'content-type: application/json' \
  -d '{"image":"test","labels":{"team":"o1"},"cpu":1,"memoryMib":512,"network":{"mode":"open"}}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
api -X POST "http://d/v1/sandboxes/$open_id/start" >/dev/null
api -X POST "http://d/v1/sandboxes/$open_id/exec" -H 'content-type: application/json' \
  -d '{"argv":["/bin/sh","-c","wget -T 3 -q -O - http://169.254.169.254/ 2>/dev/null; wget -T 3 -q -O - https://169.254.169.254/ 2>/dev/null; true"],"timeoutMs":15000}' >/dev/null
sleep 0.5
access=$(api "http://d/v1/access-log?sinceMs=0&untilMs=9999999999999")
check access-log-host "$access" '"host":"169.254.169.254"'
check access-log-denied "$access" '"decision":"denied"'
check access-log-labels "$access" '"team":"o1"'
check access-log-sandbox "$access" "\"sandboxId\":\"$open_id\""
api -X DELETE "http://d/v1/sandboxes/$open_id" >/dev/null
echo SMOKE_PASS
