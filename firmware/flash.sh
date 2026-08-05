#!/usr/bin/env bash
# Build a firmware binary and push it to a Teensy 4.1.
#
#   ./flash.sh bench    # cycle budget harness (start here)
#   ./flash.sh firmware # audio binary
#
# Prerequisites:
#   rustup target add thumbv7em-none-eabihf
#   rustup component add llvm-tools-preview
#   cargo install cargo-binutils
#   teensy_loader_cli  (https://www.pjrc.com/teensy/loader_cli.html)

set -euo pipefail

BIN="${1:-bench}"
PROFILE="${2:-release}"
TARGET_DIR="target/thumbv7em-none-eabihf/${PROFILE}"

echo "==> building ${BIN} (${PROFILE})"
if [ "$PROFILE" = "release" ]; then
    cargo build --release --bin "$BIN"
else
    cargo build --bin "$BIN"
fi

echo "==> converting to HEX"
rust-objcopy -O ihex "${TARGET_DIR}/${BIN}" "${TARGET_DIR}/${BIN}.hex"

# Worth watching. The Teensy 4.1 has 8MB of flash, so you will not run out,
# but a sudden jump usually means something pulled in formatting machinery or
# a panic path you did not intend.
SIZE=$(stat -f%z "${TARGET_DIR}/${BIN}.hex" 2>/dev/null || stat -c%s "${TARGET_DIR}/${BIN}.hex")
echo "    ${BIN}.hex is ${SIZE} bytes"

echo "==> flashing (press the button on the Teensy if it does not go automatically)"
teensy_loader_cli --mcu=TEENSY41 -w -v "${TARGET_DIR}/${BIN}.hex"

echo
echo "Done. For log output:"
echo "    screen /dev/ttyACM0 115200        # Linux"
echo "    screen /dev/cu.usbmodem* 115200   # macOS"
