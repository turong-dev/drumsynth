# Launch Control XL 3 — DAW mode

What the controller puts on the wire, what we send it, and what is still
unconfirmed. Background and the UX this serves: issue #13.

Everything below is on the **DAW In/Out** USB port. DAW mode sends nothing
on the DIN outputs and nothing on the main MIDI In/Out interface.

Source: [programmer's reference guide v1.0][ref], plus Novation's
[DAW mode][daw], [SysEx format][sysex], [feature controls][feat] and
[relative encoders][rel] articles.

[ref]: https://fael-downloads-prod.focusrite.com/customer/prod/downloads/launch_control_xl_3_programmer_s_reference_guide-pdf_en.pdf
[daw]: https://userguides.novationmusic.com/hc/en-gb/articles/27840466544402-Launch-Control-XL-3-programmer-s-DAW-mode
[sysex]: https://userguides.novationmusic.com/hc/en-gb/articles/27840466455314-SysEx-message-format-used-by-Launch-Control-XL-3
[feat]: https://userguides.novationmusic.com/hc/en-gb/articles/27840466607890-Launch-Control-XL-3-feature-Controls
[rel]: https://support.novationmusic.com/hc/en-gb/articles/38440851693458-Using-Relative-Encoders

## Reading the reference

Two things about the source document are worth knowing before trusting
anything transcribed from it.

The prose is extractable text. **The control map is not** — pages 9 and 10
are rendered 3D images of the panel with the CC number printed under each
control, and the colour palette on page 12 is an image too. Every control
index in this document was read off those renders at 4× zoom, in both the
decimal and hexadecimal versions, and the two agreed.

That is still OCR-of-a-perspective-render, so it gets one independent
check: the reference *does* state the relative-mode encoder CCs in prose
(row 1 = 77–84, row 2 = 85–92, row 3 = 93–100) and says they are the
absolute indices plus 64. 77 − 64 = 13 and 100 − 64 = 36, which is exactly
the encoder block read off the diagram. `relative_rows_are_the_absolute_
indices_plus_64` in `render/src/lcxl3/wire.rs` holds that check in the test
suite.

The palette table got no such check and so is not used: LED colour goes out
as explicit RGB SysEx instead. See *Colour*, below.

## Control map

| Control | Channel | CC / index |
|---|---|---|
| Faders, left to right | 16 | 5–12 |
| Encoders, reading order | 16 | 13–36 (row 1: 13–20, row 2: 21–28, row 3: 29–36) |
| Encoders in relative mode | 16 | absolute + 64 → 77–84, 85–92, 93–100 |
| Upper button row ("Solo/Arm") | 1 | 37–44 |
| Lower button row ("Mute/Select") | 1 | 45–52 |
| Page ▲ / ▼ | 1 | 106 / 107 |
| Track ◀ / ▶ ("Settings") | 1 | 101 / 102 |
| Play / Record | 1 | 116 / 118 |
| Shift | 7 | 63 |
| Touch events | 15 | the control's **absolute** index, 127 = on, 0 = off |
| Mode report and select | 7 | 30 |

One numbering does three jobs: the CC a control reports on, the index an
LED colour message addresses, and the per-control display target. Encoder
LEDs keep using the *absolute* index even when the encoder is in relative
mode.

#13 uses the upper row for track select and the lower row for the eight
`ParamGroup::ADDRESSABLE` entries — eight buttons, eight groups, which is
not a coincidence.

## Messages we send

All SysEx starts `F0 00 20 29 02 15`.

| Purpose | Bytes |
|---|---|
| Enter DAW mode | `F0 00 20 29 02 15 02 7F F7` (or note `9F 0C 7F`) |
| Leave DAW mode | `F0 00 20 29 02 15 02 00 F7` (or note `9F 0C 00`) |
| LED colour, RGB | `F0 00 20 29 02 15 01 53 <idx> <R> <G> <B> F7` |
| LED colour, palette | `B0 <idx> <palette>` |
| Configure a display | `F0 00 20 29 02 15 04 <target> <config> F7` |
| Set display text | `F0 00 20 29 02 15 06 <target> <field> <text…> F7` |

Display targets: `05h`–`24h` are the per-control temporary displays (same
numbering as the controls), `35h` the stationary display, `36h` the global
overlay.

The config byte is `0` to cancel and `7Fh` to trigger with current
contents; otherwise bit 6 is "auto temporary display on change", bit 5 is
"auto on touch" (both default **set**), and bits 0–4 are the arrangement.
The driver clears both auto bits: the controller's own temporary display
shows the raw CC and a number out of 127, and we are about to send better.

Arrangements used here:

- **3** — "1 line + 2×4: Title and 8 names", 9 fields. Field 0 is the
  title, 1–8 the names under one row of eight encoders. This is the
  stationary display.
- **4** — "Parameter Name and Numeric Parameter Value", the per-control
  default, for the temporary display a turn raises.

Text is ASCII `20h`–`7Eh`, plus four reassigned control codes (`1Bh` empty
box, `1Ch` filled box, `1Dh` flat, `1Eh` heart) that nothing here uses.

### Feature controls (channel 7)

Sent to channel 7; the device answers on channel 7 and accepts queries on
channel 8.

| CC | Feature | Values |
|---|---|---|
| 30 | Surface mode | 1 = DAW Mixer, 2 = DAW Control, 6–9 / 18–29 = custom modes |
| 63 | Shift | on/off |
| 69 / 72 / 73 | Relative output, encoder rows 1 / 2 / 3 | on/off |
| 70 | Fader pickup | on/off |
| 71 | Touch events | on/off |
| 100 | Global MIDI channel | 0–15 |
| 111 | LED brightness | 0–127 |
| 112 | Screen brightness | 0–127 |
| 113 | Temporary display timeout | 0–99, tenths of a second, 0 means 1 s |
| 120 | Out2 MIDI thru | on/off |
| 121 | Encoder curve | 0 slow, 1 medium, 2 fast |

## Colour

LED colour can go out as a palette index in three bytes (`B0 <idx>
<palette>`) or as explicit RGB in thirteen. The driver uses RGB.

The reason is not bandwidth, it is that the palette table in the reference
is a rendered image and the version of it we could extract is the index
numbers *without* their swatches. A palette index in the source would be a
number we could not justify. RGB says what it means, and the group colours
then live in `GROUP_COLOURS` in `render/src/lcxl3/surface.rs` as readable
values. Components are 7-bit, as every SysEx data byte must be.

If a capture later shows the repaint burst is too wide, the palette is
where to save three quarters of it — but measure first, and get the
swatches first.

## Phase 0: what to capture, and what each capture decides

`render monitor --daw --hex` puts the controller into DAW mode with
relative rows, touch events and fader pickup on, then prints everything
that arrives. It takes the device back out of DAW mode on exit.

```bash
cargo run -p render --features live -- monitor --daw --hex --port LCXL3
```

Five things to settle, in rough order of how much depends on them.

**1. What a fast turn of a relative encoder sends.** The reference gives
the pivot and the unit steps — `41h` (65) is +1, `3Fh` (63) is −1 — and
says nothing about speed. Either the magnitude grows with speed (binary
offset: 70 is +6) or the magnitude stays 1 and the messages simply arrive
more often. Turn one encoder slowly, then as fast as you can, at each of
the three encoder-curve settings (feature CC 121 = 0, 1, 2), and record the
data bytes.

This decides whether the driver can pass the delta straight through to
CC 96/97 as a step size or has to apply its own acceleration. It lands in
exactly one function, `wire::relative_delta`, which currently assumes the
first reading. The state machine above it is written in detents and does
not care either way.

**2. Hold threshold.** `surface::HOLD_MS` is 300 ms, inherited from the
removed grid surface, where it was the threshold for a *pad*. A button
under a finger that is already reaching for an encoder may want less. Tune
it by feel against the hardware.

**3. Repaint rate.** Phase 2 of #13 asks for a measured limit, and
`driver::RENDER_INTERVAL_MS` (10 ms) and `driver::MAX_SENDS_PER_RENDER`
(12) are guesses. A worst-case transition is a title, eight names and up to
forty LED writes. Drive it hard — hold a group button and sweep an encoder
while paging — and watch for the screen falling behind or messages being
dropped. The grid's `RENDER_INTERVAL_MS` / `MAX_SENDS_PER_RENDER` pacing is
the pattern being copied; its `REFRESH_MS` full-resync burst is the one
deliberately *not* copied.

**4. What leaving DAW mode by hand looks like.** Press Mode on the device
and confirm the ch 7 CC 30 report arrives, and with what value. The driver
re-enters DAW mode and repaints from scratch whenever it sees a mode that
is not DAW Control, which needs the report observed rather than assumed.

**5. The display-target overlap.** The reference says targets `05h`–`24h`
(5–36) are the per-control temporary displays, and separately that "the
arrangement is ignored for targets only setting names (`22h`(34) –
`28h`(40))". Those two ranges overlap at 34–36, which are encoders 22–24.
One of the two statements is wrong. Set text on target 36 and see which.

## Not yet confirmed

- Everything in *Phase 0*, above.
- The upper/lower button rows are **assumed** to be 37–44 and 45–52
  respectively, read off the diagram. If they turn out to be swapped, the
  fix is two constants in `wire.rs`; nothing above them changes.
- `Control::from_index` deliberately does not decode absolute-mode encoder
  messages (13–36). The driver puts every row into relative mode at
  startup, so an absolute message means the mode change did not take —
  which should show up as a dead encoder rather than be silently handled.
