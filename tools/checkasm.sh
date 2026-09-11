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

# `--mcpu=cortex-m7` is not optional. llvm-objdump picks its disassembler
# subtarget from the ELF's build attributes, and for a build carrying
# `-C target-cpu=cortex-m7` it picks one that cannot decode VFP -- every
# floating-point instruction comes back as `<unknown>`. Without this flag the
# tool reports zero FP instructions for a build that is full of them, which
# reads exactly like "the flag disabled the FPU".
echo "==> $ELF"
DIS="$(rust-objdump -d --mcpu=cortex-m7 "$ELF")"

UNKNOWN="$(printf '%s\n' "$DIS" | grep -c '<unknown>' || true)"
if [ "$UNKNOWN" -gt 64 ]; then
    echo "  warning: $UNKNOWN undecoded instructions -- counts below are unreliable" >&2
fi

# Count occurrences of a mnemonic in the instruction column only, so that
# data bytes and symbol names cannot inflate the total.
MNEMONICS="$(printf '%s\n' "$DIS" | awk -F'\t' 'NF>1 {print $2}' | awk '{print $1}')"

count() {
    # $1 = label, $2 = extended regex anchored at the mnemonic
    local n
    n="$(printf '%s\n' "$MNEMONICS" | grep -cE "^$2" || true)"
    printf '  %-28s %s\n' "$1" "$n"
}

# Calls are matched against the full disassembly, since the callee name lives
# in the operand column.
count_call() {
    local n
    n="$(printf '%s\n' "$DIS" | grep -cE "\bbl\b.*$2" || true)"
    printf '  %-28s %s\n' "$1" "$n"
}

echo
echo "floating point:"
count "total FP instructions"   'v'
count "vfma/vfms (fused MAC)"  'vfm[as]'
count "vmla/vmls (chained MAC)" 'vml[as]'
count "vmul"                    'vmul'
count "vdiv"                    'vdiv'
count "vsqrt"                   'vsqrt'
count "vabs"                    'vabs'
count "vldr/vstr (FP mem)"      'v(ldr|str)'

echo
echo "libm calls remaining (want these at or near zero):"
for sym in sinf cosf floorf ceilf powf expf logf fmodf roundf; do
    count_call "bl <...${sym}>" "${sym}"
done

echo
echo "reboot path:"
count "bkpt (autoboot -> HalfKay)" 'bkpt'

echo
echo "section sizes:"
rust-size -A "$ELF" | awk '
    /^\.(boot|text|rodata|data|bss|uninit|stack|heap|vector_table)/ {
        printf "  %-16s %10d\n", $1, $2
    }'
