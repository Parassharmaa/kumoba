#!/usr/bin/env bash
# Called by scripts/smoke-daemon.sh with the Rust CLI and by scripts/ci-sdk.sh with the npm
# CLI, against a running kumobad that KUMOBA_URL and KUMOBA_TOKEN point at. Both CLIs build
# their commands from the daemon's catalog and must behave the same on a real VM.
#   scripts/cli-smoke.sh <cli command...>      e.g. scripts/cli-smoke.sh .local/bin/kumoba
set -euo pipefail
cli=("$@")
image=${KUMOBA_SMOKE_IMAGE:-test}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/kumoba-cli.XXXXXX")
id=""
trap '[[ -n "$id" ]] && "${cli[@]}" delete "$id" >/dev/null 2>&1; rm -rf "$scratch"' EXIT
check() { if [[ "$2" != "$3" ]]; then printf 'FAIL %s: expected [%s], got [%s]\n' "$1" "$3" "$2" >&2; exit 1; fi; printf 'ok   cli %s\n' "$1"; }
status() { set +e; "$@" > "$scratch/out" 2> "$scratch/err"; local code=$?; set -e; echo "$code"; }

check catalog "$("${cli[@]}" commands | grep -c '^exec ')" 1
id=$("${cli[@]}" create --image "$image" --memory-mib 512 -q)
check create "$("${cli[@]}" get "$id" --json | grep -c '"state":"stopped"')" 1
"${cli[@]}" start "$id" -q > /dev/null
check exec-exit "$(status "${cli[@]}" exec "$id" --user kumoba -- sh -c 'echo out; echo err >&2; exit 7')" 7
check exec-stdout "$(cat "$scratch/out")" out
check exec-stderr "$(cat "$scratch/err")" err
check exec-env "$("${cli[@]}" exec "$id" env=GREETING=hi -- sh -c 'echo $GREETING')" hi
printf 'payload' | "${cli[@]}" write-file "$id" --path /tmp/cli.txt > /dev/null
check file-roundtrip "$("${cli[@]}" read-file "$id" path=/tmp/cli.txt)" payload
check list-files "$("${cli[@]}" list-files "$id" --path /tmp --json | grep -c '"cli.txt"')" 1
check run "$("${cli[@]}" run --image "$image" -- echo one-shot)" one-shot
check run-cleans-up "$("${cli[@]}" list -q | grep -vc "^$id\$" || true)" 0
check usage-error "$(status "${cli[@]}" exec)" 2
check daemon-error "$(status "${cli[@]}" get no-such-sandbox)" 1
check unreachable "$(KUMOBA_URL=unix:///nonexistent/kumobad.sock status "${cli[@]}" images)" 3
check delete "$("${cli[@]}" delete "$id" --json)" '{"deleted":true}'
id=""
echo CLI_SMOKE_PASS
