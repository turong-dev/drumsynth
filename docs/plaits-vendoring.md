# Plaits vendoring notes

What differs between `mi-dsp/vendor/plaits` and upstream, and why.

## Local modification: `SyntheticBassDrum::Init` misses two fields

Upstream's `Init()` sets eleven of the thirteen members of
`plaits::SyntheticBassDrum`. It leaves out `transient_env_` and
`transient_env_lp_`.

`transient_env_` is assigned on every trigger (`body_env_ = transient_env_ =
0.3f + 0.7f * accent`), so in practice it is written before it matters.
`transient_env_lp_` is not: it is a `ONE_POLE` accumulator, read and rewritten
from its own previous value on the very first sample of the very first render,
and nothing else ever assigns it. The voice therefore started from whatever
was in that memory and smoothed away from it over the following milliseconds.

As with the Peaks hi-hat, upstream never sees this because the object is a
zeroed static initialised once at boot. Here the engine is heap-allocated and
slots are re-initialised when their machine changes, so the field picked up
foreign bytes and the render differed between processes.

The fix is two lines in `Init`, marked `LOCAL FIX` in
`vendor/plaits/dsp/drums/synthetic_bass_drum.h`. Re-apply it if the vendored
tree is ever refreshed from upstream.

**This one is not caught by `slot_reuse`**, and the reason is worth recording:
by the end of a preceding hit `transient_env_lp_` has decayed to approximately
zero, so the stale value and the correct value are indistinguishable within a
process. It was found by running the baseline digest in separate processes and
bisecting which machine window diverged. `DESIGN.md` ("mi-drum determinism")
has the procedure.

## `kMaxEngines` is 24, but `RegisterInstance` is called 28 times

Unmodified, and noted here because it looks like a bug and is load-bearing.
`voice.h` sizes the registry at 24 while `voice.cc` registers 28 engines, so
the last four are silently dropped and indices 24-27 clamp to 23 (hi-hat).
`MiMachineId` maps the four Peaks ids to exactly 24-27, so anything that routed
a Peaks id into the Plaits path would get a hi-hat rather than an error. Safe
today only because the quantiser clamps.
