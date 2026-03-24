#!/usr/bin/env bash
# Builds the boot image, runs QEMU with serial to a temp file, greps for expected boot lines.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

IMAGE="${IMAGE:-$ROOT/target/x86_64-unknown-none/debug/bootimage-rockos.bin}"
OUT="$(mktemp)"
cleanup() { rm -f "$OUT"; }
trap cleanup EXIT

cargo build
cargo bootimage

qemu-system-x86_64 \
  -drive "format=raw,file=$IMAGE" \
  -serial "file:$OUT" \
  -display none \
  -no-reboot &
QEMU_PID=$!

for _ in $(seq 1 80); do
  if grep -q "Loaded init ELF" "$OUT" 2>/dev/null && grep -q "\[syscall\] exit(0)" "$OUT" 2>/dev/null; then
    kill "$QEMU_PID" 2>/dev/null || true
    wait "$QEMU_PID" 2>/dev/null || true
    echo "ok: serial contained expected lines"
    exit 0
  fi
  sleep 0.1
done

kill "$QEMU_PID" 2>/dev/null || true
wait "$QEMU_PID" 2>/dev/null || true
echo "--- serial capture ---"
cat "$OUT"
echo "----------------------"
exit 1
