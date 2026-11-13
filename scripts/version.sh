#!/usr/bin/env bash
# Prints the shared version from the Cargo workspace. Used by the release workflow.
set -euo pipefail
cd "$(dirname "$0")/.."
perl -0ne 'print $1 if /^\[workspace\.package\]\n(?:.*\n)*?version = "([^"]+)"/m' Cargo.toml
