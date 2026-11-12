#!/usr/bin/env bash
# Called by hand before installing kumoba on a server. Reports whether this host can
# run microVMs and how many 4 GiB sandboxes fit. Exits 1 when something blocks it.
set -uo pipefail
status=0
pass() { printf 'ok    %s\n' "$1"; }
warn() { printf 'warn  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; status=1; }

os=$(uname -s); arch=$(uname -m)
case "$os/$arch" in
  Darwin/arm64) pass "platform: macOS on Apple silicon (Hypervisor.framework)" ;;
  Linux/x86_64|Linux/aarch64) pass "platform: Linux $arch" ;;
  *) fail "platform: $os/$arch is not supported (Linux x86_64/arm64 with KVM, or macOS on Apple silicon)" ;;
esac

if [[ "$os" == Linux ]]; then
  if [[ ! -e /dev/kvm ]]; then
    fail "kvm: /dev/kvm is missing. Use bare metal or a VM with nested virtualization (AWS: C8i/M8i/R8i with nested virtualization enabled, or a .metal instance)"
  elif [[ -r /dev/kvm && -w /dev/kvm ]]; then
    pass "kvm: /dev/kvm is readable and writable"
  else
    fail "kvm: $(id -un) cannot open /dev/kvm. Run: sudo usermod -aG kvm $(id -un), then log in again"
  fi
  if [[ "$arch" == x86_64 ]]; then
    if grep -qE '^flags.*\b(vmx|svm)\b' /proc/cpuinfo; then
      pass "cpu: VT-x/AMD-V exposed"
    else
      fail "cpu: no vmx/svm flag. Enable virtualization in the BIOS or nested virtualization for this VM"
    fi
  fi
  total_kib=$(awk '/MemTotal/ {print $2}' /proc/meminfo)
  total_gib=$((total_kib / 1024 / 1024))
  fs=$(df --output=fstype . | tail -1)
else
  total_gib=$(( $(sysctl -n hw.memsize) / 1024 / 1024 / 1024 ))
  fs=apfs
fi

fits=$(( (total_gib - 2) / 4 ))
if (( fits < 1 )); then
  fail "memory: ${total_gib} GiB is not enough for one 4 GiB sandbox plus the host"
else
  pass "memory: ${total_gib} GiB, room for ${fits} sandboxes of 4 GiB (set KUMOBAD_MAX_RUNNING accordingly)"
fi

free_gib=$(df -Pk . | awk 'NR==2 {print int($4/1024/1024)}')
if (( free_gib < 20 )); then
  warn "disk: ${free_gib} GiB free here; a default image needs about 4 GiB plus per-sandbox copies"
else
  pass "disk: ${free_gib} GiB free"
fi

case "$fs" in
  xfs|btrfs|apfs|zfs|bcachefs) pass "filesystem: $fs clones sandbox disks instantly" ;;
  *) warn "filesystem: $fs copies the whole image for every sandbox; xfs or btrfs makes this instant" ;;
esac

if [[ "$os" == Linux ]]; then
  for lib in "${KUMOBAD_LIBKRUN:-}" "${KUMOBAD_LIBKRUNFW:-}"; do
    [[ -n "$lib" && ! -f "$lib" ]] && fail "library: $lib does not exist"
  done
  if [[ -z "${KUMOBAD_LIBKRUN:-}" ]] && ! ldconfig -p 2>/dev/null | grep -q libkrun.so.1; then
    warn "libkrun: not found on the library path; run scripts/prepare-linux-host.sh or set KUMOBAD_LIBKRUN"
  fi
fi

exit $status
