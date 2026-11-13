#!/usr/bin/env bash
# Called by .github/workflows/ci.yml and release.yml before scripts/ci-lint.sh. actionlint
# is a Go program; go install pins the version and checks it against the Go checksum database.
set -euo pipefail
go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12
echo "$(go env GOPATH)/bin" >> "$GITHUB_PATH"
