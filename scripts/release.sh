#!/usr/bin/env bash
# Run by hand on a clean main checkout to cut a release: sets the shared version in the
# Cargo workspace and the npm package, turns the Unreleased changelog section into the
# release section, commits and tags. Pushing the tag starts .github/workflows/release.yml.
#   scripts/release.sh <version>      e.g. scripts/release.sh 0.1.0
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
version=${1:?usage: release.sh <version>}
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || { echo "not a semver version: $version" >&2; exit 2; }
[[ -z "$(git status --porcelain)" ]] || { echo 'working tree must be clean' >&2; exit 2; }
[[ "$(git branch --show-current)" == main ]] || { echo 'release from main' >&2; exit 2; }
git tag -l "v$version" | grep -q . && { echo "tag v$version already exists" >&2; exit 2; }
grep -q '^## \[Unreleased\]$' CHANGELOG.md || { echo 'CHANGELOG.md needs an Unreleased section' >&2; exit 2; }
previous=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
today=$(date -u +%Y-%m-%d)

perl -0pi -e "s/^(\[workspace\.package\]\n(?:.*\n)*?)version = \"[^\"]+\"/\$1version = \"$version\"/m" Cargo.toml
cargo update --workspace --offline --quiet
(cd sdk/typescript && npm version "$version" --no-git-tag-version --allow-same-version >/dev/null)

perl -pi -e "s/^## \[Unreleased\]\$/## [Unreleased]\n\n## [$version] - $today/" CHANGELOG.md
if [[ -n "$previous" ]]; then
  perl -pi -e "s{^\[Unreleased\]: .*\$}{[Unreleased]: https://github.com/Parassharmaa/kumoba/compare/v$version...HEAD\n[$version]: https://github.com/Parassharmaa/kumoba/compare/$previous...v$version}" CHANGELOG.md
else
  perl -pi -e "s{^\[Unreleased\]: .*\$}{[Unreleased]: https://github.com/Parassharmaa/kumoba/compare/v$version...HEAD\n[$version]: https://github.com/Parassharmaa/kumoba/releases/tag/v$version}" CHANGELOG.md
fi

scripts/release-check.sh "v$version"
git add Cargo.toml Cargo.lock CHANGELOG.md sdk/typescript/package.json sdk/typescript/package-lock.json
git commit --quiet -m "Release v$version"
git tag -a "v$version" -m "kumoba v$version"
printf 'Tagged v%s. Publish with:\n  git push origin main v%s\n' "$version" "$version"
