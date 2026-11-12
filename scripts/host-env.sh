#!/usr/bin/env bash
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:/opt/homebrew/opt/e2fsprogs/sbin:/usr/sbin:$PATH"
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) image_arch=arm64; alpine_arch=aarch64; host_os=macos ;;
  Linux/aarch64) image_arch=arm64; alpine_arch=aarch64; host_os=linux ;;
  Linux/x86_64) image_arch=x86_64; alpine_arch=x86_64; host_os=linux ;;
  *) echo 'Supported hosts: Apple Silicon macOS, Linux ARM64/x86_64 with KVM.' >&2; exit 2 ;;
esac
guest_target="$alpine_arch-unknown-linux-musl"
verify_sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum -c -
  else
    shasum -a 256 -c -
  fi
}
stage_worker() {
  mkdir -p .local/bin
  cp target/release/kumoba-worker .local/bin/kumoba-worker.next
  if [[ "$host_os" == macos ]]; then
    codesign --force --sign - --entitlements scripts/hvf-entitlements.plist .local/bin/kumoba-worker.next
  fi
  mv .local/bin/kumoba-worker.next .local/bin/kumoba-worker
}
