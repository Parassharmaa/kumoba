# Releasing

kumoba ships as four crates on crates.io (`kumoba-protocol`, `kumoba-guest`, `kumoba`, `kumobad`), one npm package (`kumoba`) and a GitHub release with host tarballs. They share one version and go out together from one tag.

## Versioning

- One version for everything. The SDK version tells you which `kumobad` it was tested against, and the `kumobad` version tells you which guest agent its images need.
- Pre-1.0: a minor bump (`0.2.0`) may break the HTTP API, the SDK or the host-guest protocol; a patch bump never does.
- Bumping `VERSION` in `crates/kumoba-protocol/src/lib.rs` is always a minor bump, and the changelog entry says that images must be rebuilt. The worker refuses a guest with another protocol version, so a stale image fails loudly rather than subtly.
- Every pull request that changes behaviour adds a line under `## [Unreleased]` in `CHANGELOG.md`. The release notes are generated from that section, so write for users, not for the diff.

## Cutting a release

On a clean `main` checkout that CI has passed:

```sh
scripts/release.sh 0.2.0
git show --stat HEAD
git push origin main v0.2.0
```

`release.sh` writes the version into `Cargo.toml`, `Cargo.lock`, `package.json` and `package-lock.json`, turns the Unreleased section into `## [0.2.0] - <date>` with compare links, runs `scripts/release-check.sh`, commits `Release v0.2.0` and tags `v0.2.0`. It pushes nothing.

Pushing the tag runs `.github/workflows/release.yml`:

1. **check**: `scripts/ci-release.sh` confirms the tag, manifests and changelog agree, runs the unit tests, `cargo publish --workspace --dry-run` and the SDK typecheck, build, tests and `npm pack --dry-run`.
2. **assets**: builds `kumoba-<version>-linux-x86_64.tar.zst`, `-linux-arm64.tar.zst` and `-macos-arm64.tar.zst` with `scripts/release-assets.sh`. Linux tarballs carry the patched libkrun and libkrunfw; the macOS tarball expects Homebrew's.
3. **publish-crates**: `scripts/publish-crates.sh` publishes the crates in dependency order with a short-lived token from crates.io trusted publishing.
4. **publish-npm**: `scripts/publish-npm.sh` publishes `kumoba` to npm through trusted publishing, which also attaches a provenance attestation.
5. **release**: `scripts/publish-release.sh` creates the GitHub release with the changelog section, the generated contributor notes, the tarballs and `SHA256SUMS`.

Every publish step skips a version the registry already has, so rerunning a failed workflow is safe. Running the workflow by hand from the Actions tab does steps 1 and 2 only, which is the way to test the pipeline without a tag.

## When something goes wrong

- A release that must not be used: `cargo yank --version X.Y.Z <crate>` for each crate and `npm deprecate kumoba@X.Y.Z "<reason>"`, then ship a fixed patch release. Nothing is ever unpublished or re-tagged.
- A broken tarball with good packages: fix the script, rerun the workflow. `publish-release.sh` replaces assets on an existing release.
- A tag on the wrong commit that has not been pushed: `git tag -d vX.Y.Z` and `git reset --hard HEAD~1`, then start again.

## One-time setup

Both registries authenticate the workflow with GitHub's OIDC token instead of a stored secret, and both configure that per package, so the first version of each package is published by hand and the workflow's publish jobs run for real from the second release on.

1. First release only, from the tagged commit before pushing the tag: `cargo login`, then `cargo publish --workspace`; `cd sdk/typescript && npm login && npm publish`.
2. crates.io: for each of the four crates, Settings, Trusted Publishing, add GitHub with owner `Parassharmaa`, repository `kumoba`, workflow `release.yml`. The crates.io documentation at https://crates.io/docs/trusted-publishing is the reference for the exact fields.
3. npm: package `kumoba`, Settings, Trusted publisher, GitHub Actions with the same owner, repository and workflow filename. Then set publishing access to require two-factor authentication and disallow tokens; trusted publishers are unaffected. npm expires a trusted publisher that has not published within two days of being configured, so do this right before the second release rather than after the first.
4. Push the tag. The workflow finds every version already on its registry, skips the crates.io and npm jobs without requesting a token, and still builds the tarballs and the GitHub release.
5. Done already: repository rulesets stop `v*` tags from being moved or deleted and stop force pushes to or deletion of `main`. A pushed tag is a release, so these stay on.
