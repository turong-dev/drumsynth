#!/usr/bin/env bash
# Confirm a build flag actually changed codegen, independently of cycle counts.
#
#   tools/checkasm.sh                      # the release bench ELF
#   tools/checkasm.sh path/to/other.elf
#
# A flag that measures as noise might have done nothing at all -- e.g. a
# `-C target-cpu` that never reached rustc, or a `mul_add` that still lowered
# to a call. This counts the instructions that answer that question.
#
# What to look for on a Cortex-M7:
#
#   vfma/vfms   fused multiply-add. Absent almost everywhere on the default
#               `thumbv7em-none-eabihf` CPU model; should appear once
#               `-C target-cpu=cortex-m7` is in the rustflags.
#   vdiv        hardware divide. Data-dependent latency on the M7, which is
#               why `dsp::fast::recip` exists. Watch this if you swap the
#               Newton iteration back out for a real divide.
#   bl <sinf>   any libm call surviving in the audio path. `sin_turns` is a
#   bl <floorf> table lookup and should never emit one; `exp2_approx` still
#               calls `floorf` today, which is the one known offender.
#
# Note that this counts *static* instruction occurrences, not executions. It
# tells you whether the compiler can emit something, not how hot it is -- that
# is what benchloop.py is for.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ELF="${1:-$REPO/firmware/target/thumbv7em-none-eabihf/release/bench}"

if [ ! -f "$ELF" ]; then
    echo "no ELF at $ELF" >&2
    echo "build one first:  cd firmware && cargo build --release --bin bench" >&2
    exit 1
fi

echo "==> $ELF"
DIS="$(rust-objdump -d "$ELF")"

count() {
    # $1 = label, $2 = extended regex over the mnemonic column
    local n
    n="$(printf '%s\n' "$DIS" | grep -cE "$2" || true)"
    printf '  %-28s %s\n' "$1" "$n"
}

echo
echo "floating point:"
count "vfma/vfms (fused MAC)"  '\bvfm[as]'
count "vmla/vmls (chained MAC)" '\bvml[as]'
count "vdiv"                    '\bvdiv'
count "vsqrt"                   '\bvsqrt'
count "vabs"                    '\bvabs'

echo
echo "libm calls remaining (want these at or near zero):"
for sym in sinf cosf floorf ceilf powf expf logf fmodf roundf; do
    count "bl <...${sym}>" "\bbl\b.*${sym}"
done

echo
echo "reboot path:"
count "bkpt (autoboot -> HalfKay)" '\bbkpt'

echo
echo "section sizes:"
rust-size -A "$ELF" | awk '
    /^\.(boot|text|rodata|data|bss|uninit|stack|heap|vector_table)/ {
        printf "  %-16s %10d\n", $1, $2
    }'
