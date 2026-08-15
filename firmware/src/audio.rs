//! SAI1 I2S audio output to the PCM5102A DAC.
//!
//! # Wiring
//!
//! ```text
//!   Teensy 4.1              PCM5102A breakout
//!   ──────────              ─────────────────
//!   pin 21 (BCLK)   ──────► BCK
//!   pin 20 (LRCLK)  ──────► LCK
//!   pin 7  (OUT1A)  ──────► DIN
//!   3.3V            ──────► VIN
//!   3.3V            ──────► A3V3   (if exposed — see below)
//!   3.3V            ──────► XSMT   (if exposed — see below)
//!   GND             ──────► GND
//!   GND             ──────► SCK    (ties it low — internal PLL mode)
//!   GND             ──────► AGND   (if exposed — see below)
//! ```
//!
//! Most PCM5102A breakouts want SCK tied low to select internal PLL mode;
//! the SAI drives BCLK/LRCLK directly, so no MCLK pin is wired (and none is
//! needed — the SAI1 clock root feeds BCLK through the on-chip divider, the
//! same way the Teensy Audio Library runs without an MCLK output).
//!
//! The table above covers the bare-minimum breakout (the common small
//! boards that tie everything else internally). Boards that break out more
//! of the PCM5102A's own pins need three more connections, none of them
//! optional — miss any one and the result is total, otherwise-unexplained
//! silence despite BCLK/LRCLK/DATA all being correct:
//!
//! - **AGND** — the chip's analog ground, separate from digital `GND`.
//!   Some boards tie this internally and don't expose it; others break it
//!   out and leave it floating until you wire it. Tie it to the same
//!   ground as everything else.
//! - **A3V3** — the analog supply (`AVDD`), separate from the digital
//!   `VIN`. Without it the output stage has no power at all. Tie it to the
//!   same 3.3V rail as `VIN`.
//! - **XSMT** — soft-mute control. Low or floating means muted, full stop,
//!   regardless of how correct the I2S stream is. Tie it to 3.3V.
//!
//! `FLT` (filter select) and `DEMP` (de-emphasis select) pick cosmetic
//! filter/curve variants, not mute/power — tie both to GND for the
//! standard behaviour if your board exposes them, but leaving them
//! floating won't cause silence the way AGND/A3V3/XSMT will.
//!
//! `FMT` (audio data format select) is a different story: low selects I2S,
//! high selects left-justified. This firmware's `SaiConfig::i2s(...)` is
//! the I2S protocol specifically (the sample's MSB lands one BCLK after
//! the LRCLK edge, not on it) — if FMT floats to (or is wired for)
//! left-justified, every sample lands shifted by one bit position, which
//! reads as silence or noise despite BCLK/LRCLK/DATA all being otherwise
//! correct. Tie FMT to GND.
//!
//! # Clock chain — exactly 48 kHz
//!
//! The Teensy Audio Library targets PLL4 = fs × 256 × 4 × 14:
//!
//! | stage            | value            | derivation |
//! |------------------|------------------|------------|
//! | PLL4             | 688,128,000 Hz   | 24e6 × (28 + 6720/10000) |
//! | SAI1_CLK root    | 12,288,000 Hz    | PLL4 / prediv 4 / podf 14 |
//! | BCLK             | 3,072,000 Hz     | SAI1_CLK / 4 (bclk_div 4 → DIV=1) |
//! | LRCLK            | 48,000 Hz        | BCLK / (2 channels × 32 bits) |
//!
//! **`MSEL` must be set to `Select1` on the RX side, explicitly** —
//! `SaiConfig::i2s()` defaults `mclk_source` to `MclkSource::Sysclk`, which
//! routes the bit-clock divider off the raw ~150MHz IPG bus clock instead of
//! the `SAI1_CLK`/PLL4 chain above. RX is our clock generator (pins 20/21
//! per the wiring table), so that default silently produces a BCLK/LRCLK at
//! the wrong frequency entirely (bus clock / bclk_div, nowhere near
//! 3.072MHz) — one that still toggles, still drains the TX FIFO, and still
//! reads as "enabled" in every status register, but that the DAC's
//! auto-clock-detect PLL can never lock onto, so it sits in permanent
//! silent standby. (The driver hardcodes TX's `MSEL` to a masked value of 1
//! regardless of config — harmless, since TX follows RX's clock anyway —
//! which is what makes this asymmetric and easy to miss.) `setup()` sets
//! `cfg.mclk_source = MclkSource::Select1` before `split()` for exactly this
//! reason; do not remove it.
//!
//! Word size is 32 bits with 16 meaningful bits, left-justified (sample in
//! the high halfword, MSB at bit 31) — the layout the PCM5102's I2S decoder
//! expects and the one the Teensy Audio Library emits.
//!
//! # Why FIFO-request interrupts and not DMA
//!
//! imxrt-hal's SAI driver only lets DMA reach the lowest-numbered enabled
//! data line, and imxrt-dma's `Channel` API has no half-transfer interrupt,
//! so the planned circular double-buffer pattern is not available. The FIFO
//! path here is exactly what the upstream `rtic_sai_pcm5102` example uses on
//! this hardware: the TX FIFO is 32 words, the watermark is 16, and the
//! interrupt fires whenever the count drops to or below the watermark
//! (~every 16 samples, ~333 µs at 48 kHz) and refills until the request flag
//! clears.
//!
//! # Threading
//!
//! The SAI interrupt owns the [`DrumEngine`], the interleaved sample buffers,
//! and the sample counter. The main loop only pushes MIDI events and wraps
//! every `schedule_midi` in `cortex_m::interrupt::free` so a push cannot be
//! preempted by the ISR's drain. Nothing else runs in an interrupt context in
//! this firmware — USB and UART MIDI are polled.

use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use cortex_m::peripheral::NVIC;
use drum_engine::{DrumEngine, BLOCK};
use teensy4_bsp as bsp;

use bsp::hal::ccm::{analog::pll4, clock_gate, sai_clk};
use bsp::hal::gpio::Output;
use bsp::hal::iomuxc::sai;
use bsp::hal::sai::{bclk_div, Interrupts, Packing, Sai, SaiConfig, Status, SyncMode};
use bsp::ral::Interrupt;

/// Frame clock (LRCLK) frequency — the audio sample rate.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Diagnostic switch: when `true`, the SAI interrupt emits a fixed 440 Hz
/// sine instead of the engine's output, and never touches the engine. A
/// real sine, not a square wave — see [`AudioState::tone_osc`] for why that
/// distinction mattered here in practice, not just in theory.
///
/// The on-board LED blinks at ~1 Hz from inside the ISR either way, so one
/// flash tells you which half of the path is broken:
///
/// | LED  | tone       | meaning                                          |
/// |------|------------|--------------------------------------------------|
/// | blinks | sounds    | SAI clock + ISR + DAC all work — MIDI/engine is the fault |
/// | blinks | silent    | ISR pumps frames; DAC wiring / mode straps are wrong |
/// | solid  | (any)     | ISR fires but spins (bit clock too fast / FIFO never satisfied) |
/// | off    | —         | ISR never fires (dead clock, NVIC, or early hang)  |
///
/// Set `false` for the real instrument.
pub const TEST_TONE: bool = false;

/// The engine, shared with the main loop.
///
/// Set once by [`setup`] before interrupts are enabled; read only by the SAI
/// interrupt afterwards (and only when [`TEST_TONE`] is off). The main loop
/// keeps its own `&'static mut` and wraps `schedule_midi` in
/// `interrupt::free`, so the two contexts never overlap.
static ENGINE: AtomicPtr<DrumEngine> = AtomicPtr::new(core::ptr::null_mut());

/// The on-board LED, handed to the SAI interrupt by [`start`] so it can blink
/// from inside the ISR (a main-loop blink would go dark if the loop were
/// starved by a spinning ISR — exactly the failure we want to see).
static LED: AtomicPtr<Output> = AtomicPtr::new(core::ptr::null_mut());

/// Samples fully consumed by the DAC. The next block `process` renders
/// starts at this count. Incremented in whole blocks by the SAI interrupt;
/// read (relaxed) by the main loop to compute arrival offsets.
static SAMPLE_COUNTER: AtomicU32 = AtomicU32::new(0);

/// The SAI transmitter, owned by the SAI interrupt after [`setup`].
static mut SAI_TX: MaybeUninit<bsp::hal::sai::Tx> = MaybeUninit::uninit();

/// The SAI receiver, owned by the SAI interrupt after [`setup`].
///
/// We never want received *data* — RX exists only because its pads (20/21)
/// are the ones wired to BCLK/LRCLK, and `SyncMode::TxFollowRx` makes it the
/// bit-clock/frame-sync master TX rides on. But enabling a receive channel
/// (`rx_chan_mask` nonzero in [`setup`]) means its FIFO fills with whatever
/// arrives whether we read it or not. Left undrained, it overflows in about
/// one FIFO's worth of words (~330us at this bit rate) — and on this SAI
/// block, an unserviced full RX FIFO backpressures the *shared* bit-clock
/// generator, stalling BCLK for the whole peripheral, TX included. The ISR
/// drains and discards RX every pass specifically to keep that from ever
/// happening again.
static mut SAI_RX: MaybeUninit<bsp::hal::sai::Rx> = MaybeUninit::uninit();

/// State owned exclusively by the SAI interrupt.
struct AudioState {
    /// Current rendered block, planar (unused while [`TEST_TONE`] is on).
    left: [f32; BLOCK],
    right: [f32; BLOCK],
    /// Next sample of the current block to transmit.
    block_pos: usize,
    /// The test tone's own oscillator (only used while [`TEST_TONE`] is on).
    ///
    /// A real sine, not a square wave — deliberately the same
    /// `drum_engine::dsp::osc::SineOsc` the engine's own sine-based machines
    /// use, so this is a genuine apples-to-apples control signal against
    /// them. A hard square wave is inherently rich in high-order harmonics
    /// and legitimately sounds buzzy/harsh through any real DAC regardless
    /// of whether anything is wrong — it was a bad control signal, and cost
    /// real debugging time before that was caught.
    tone_osc: drum_engine::dsp::osc::SineOsc,
    /// Samples since the last LED toggle.
    led_counter: u32,
}

static mut AUDIO: AudioState = AudioState {
    left: [0.0; BLOCK],
    right: [0.0; BLOCK],
    block_pos: 0,
    tone_osc: drum_engine::dsp::osc::SineOsc::new(),
    led_counter: 0,
};

/// Configure SAI1 as an I2S master at exactly 48 kHz and hand the engine to
/// the SAI interrupt.
///
/// Does **not** enable the SAI or unmask its interrupt — call [`start`] once
/// the rest of the firmware is ready.
///
/// # Safety
///
/// Call exactly once, from `main`, before interrupts are enabled, with a
/// raw pointer to a fully-constructed, still-alive `engine`. All module
/// statics are written here and must not be touched again until the SAI
/// interrupt owns them.
#[allow(unsafe_code, static_mut_refs)]
pub unsafe fn setup(
    ccm: &mut bsp::ral::ccm::CCM,
    ccm_analog: &mut bsp::ral::ccm_analog::CCM_ANALOG,
    sai1: bsp::ral::sai::SAI1,
    pins: &mut bsp::pins::t41::Pins,
    engine: *mut DrumEngine,
) {
    // 1. Reconfigure the SAI1 clock root for 48 kHz. Gate the SAI off while
    //    its clock dividers change (the driver docs require it), then back on.
    //
    // (An experiment that skipped this entirely, relying on `board::t41()`'s
    // own ~44.1kHz-family default instead, made no difference — ruled out
    // as a cause. Restored to the real 48kHz target.)
    clock_gate::sai::<1>().set(ccm, clock_gate::OFF);

    // PLL4 = 24e6 × (28 + 6720/10000) = 688,128,000 Hz. This is the Teensy
    // Audio Library's `set_audioClock(28, 6720, 10000)` — the canonical
    // 48 kHz audio PLL — and nothing else in this firmware uses PLL4.
    //
    // (An experiment swapping this for the Audio Library's own 44.1kHz-family
    // fraction produced the same ~1% relative measurement spread as this one
    // — evidence that spread is a measurement artifact of the crude Arduino
    // frequency counter, not real PLL jitter. Ruled out; reverted to 48kHz.)
    pll4::reconfigure(ccm_analog, 28, 6720, 10_000, pll4::PostDivider::U1);

    // SAI1_CLK = PLL4 / prediv(4) / podf(14) = 12,288,000 Hz.
    sai_clk::set_selection::<1>(ccm, sai_clk::Selection::Pll4);
    sai_clk::set_predivider::<1>(ccm, 4);
    sai_clk::set_divider::<1>(ccm, 14);

    clock_gate::sai::<1>().set(ccm, clock_gate::ON);

    // 2. Route the pads. Pin 20 = SAI1_RX_SYNC and pin 21 = SAI1_RX_BCLK
    //    (the RX pads are the clock master — the TX half follows them), pin
    //    7 = SAI1_TX_DATA00. The RX pads need their daisy-chain register
    //    selected; `prepare` handles that from the pad's own metadata.
    sai::prepare(&mut pins.p20);
    sai::prepare(&mut pins.p21);
    sai::prepare(&mut pins.p7);

    // 3. SAI1: I2S, 32-bit slots × 2 channels, master. `Sai::without_pins`
    //    because the pin set is asymmetric (RX clock pads + a TX data pad)
    //    and no MCLK pad — `Sai::new` wants a full symmetric set. Both
    //    channel masks are channel 0. TX follows RX for frame sync, matching
    //    the Teensy Audio Library (RX is the async clock master).
    let sai = Sai::without_pins::<1>(sai1, 1, 1);
    let mut cfg = SaiConfig::i2s(bclk_div(4));
    cfg.sync_mode = SyncMode::TxFollowRx;

    // MSEL selects which clock actually feeds the bit-clock divider on each
    // side, and `SaiConfig::i2s()` defaults it to `MclkSource::Sysclk` (the
    // raw 150MHz IPG bus clock) — completely bypassing the SAI1_CLK/PLL4
    // chain configured above. The driver hardcodes TX's MSEL to a masked
    // value of 1 regardless of config (a driver quirk, harmless since TX
    // follows RX anyway), but RX's MSEL honours `cfg.mclk_source` verbatim.
    // RX is our clock generator (pins 20/21), so leaving this at the
    // default meant RX's BCLK divider was almost certainly dividing down
    // 150MHz instead of our carefully configured 12,288,000 Hz root — e.g.
    // ~37.5MHz instead of 3.072MHz, still toggling, still draining the FIFO
    // fast, utterly unrecognizable to the DAC's auto-clock-detect PLL.
    // Select1 is what the Teensy Audio Library's own `I2S_RCR2_MSEL(1)`
    // uses to route the real peripheral clock root through instead.
    cfg.mclk_source = bsp::hal::sai::MclkSource::Select1;

    // split: TX BCLK DIV=1 → BCLK = 12,288,000 / 4 = 3,072,000 Hz; 32-bit
    // words, so the frame is 64 BCLKs → LRCLK = 48,000 Hz.
    let (tx, rx) = sai
        .split(32, 2, Packing::None, &cfg)
        .expect("32-bit words with Packing::None is always valid");
    let (Some(mut tx), Some(mut rx)) = (tx, rx) else {
        unreachable!("both SAI1 channel masks were nonzero")
    };

    // TCR4.FCONT ("Frame Sync Continue on error") — not exposed by
    // `SaiConfig`/`split()` at all, so it's left at its hardware default of
    // 0 (frame-sync/BCLK generation *stops* on any FIFO error, rather than
    // continuing through it). The Teensy Audio Library's own known-working
    // `output_i2s.cpp` sets this bit on TCR4 (not RCR4). Our very first boot
    // genuinely underran the pre-filled FIFO before the reorder fix below
    // existed, and FCONT=0 is a plausible reason that could have latched
    // the clock off for good rather than recovering — set it to match the
    // proven-working reference and remove the fragility, in case a future
    // underrun (MIDI jitter, USB polling, anything) ever happens again.
    //
    // SAFETY: raw modify of a single bit `split()` already configured the
    // rest of; done before `set_enable`, so no concurrent access yet.
    #[allow(unsafe_code)]
    unsafe {
        let regs = &*bsp::ral::sai::SAI1;
        bsp::ral::modify_reg!(bsp::ral::sai, regs, TCR4, FCONT: 1);
    }

    // `SineOsc::new()` starts silent (inc=0); give the test-tone oscillator
    // an actual frequency before the ISR ever ticks it. `set_freq` isn't
    // `const fn` (a plain multiply, but not declared as one), so this can't
    // happen in `AUDIO`'s static initializer above.
    #[allow(unsafe_code, static_mut_refs)]
    unsafe {
        (*addr_of_mut!(AUDIO)).tone_osc.set_freq(440.0);
    }

    // 4. Hand the engine to the interrupt. Pre-fill the TX FIFO with a block
    //    of silence *before* enabling, so the transmitter never underruns at
    //    start-up (the reference `rtic_sai_pcm5102` example does the same;
    //    here 15 frames × 2 words stays under the 32-word FIFO). Enable the
    //    receiver first (it generates BCLK/LRCLK), then the transmitter. Both
    //    stay running once the ISR keeps the FIFO fed.
    ENGINE.store(engine, Ordering::Relaxed);
    for _ in 0..15 {
        tx.write_frame_u32(0, &[0, 0]);
    }
    rx.set_enable(true);
    tx.set_enable(true);

    // 5. Interrupt on FIFO-empty (FWF) and FIFO-below-watermark (FRF); the
    //    ISR refills until the request flag clears. Also on RX's own
    //    watermark, so a quiet TX (nothing to refill) can't leave RX
    //    unserviced between ticks — see `SAI_RX`. Not unmasked yet.
    tx.set_interrupts(Interrupts::FIFO_WARNING | Interrupts::FIFO_REQUEST);

    rx.set_interrupts(Interrupts::FIFO_WARNING | Interrupts::FIFO_REQUEST);

    // 6. Park the transmitter and receiver in the statics the ISR reads.
    SAI_TX.write(tx);
    SAI_RX.write(rx);
}

/// Enable the SAI1 interrupt and hand the on-board LED to the ISR.
///
/// The SAI block is already running from [`setup`]; this is the moment the
/// FIFO-pump ISR starts firing, so call it only after everything the ISR
/// touches is initialized. From here the ISR blinks the LED at ~1 Hz as a
/// liveness signal.
///
/// # Safety
///
/// Call exactly once, after [`setup`], from `main`. `led` must outlive `main`
/// (it does — `main` never returns).
#[allow(unsafe_code)]
pub unsafe fn start(led: &Output) {
    // SAFETY: `main` never returns, so `led` stays alive; only the ISR reads
    // the pointer, and `Output::toggle` is an atomic hardware write.
    LED.store(core::ptr::from_ref(led).cast_mut(), Ordering::Relaxed);
    // SAFETY: Interrupt::SAI1 implements InterruptNumber; only this ISR is
    // ever unmasked.
    NVIC::unmask(Interrupt::SAI1);
}

/// Where the next rendered block starts, in samples.
///
/// Always a multiple of `BLOCK` (the ISR counts in whole blocks), so the
/// offset the main loop derives from it is 0 today — events fire at the next
/// block boundary, which is the earliest a note can play.
///
/// The underlying counter is 32-bit and wraps after ~24.8 hours; the offset
/// is derived modulo `BLOCK`, so the wrap is invisible to `schedule_midi`.
pub fn sample_counter() -> u64 {
    SAMPLE_COUNTER.load(Ordering::Relaxed) as u64
}

/// The SAI1 FIFO-pump interrupt.
///
/// Fires whenever the TX FIFO drains to or below its 16-word watermark (or
/// RX's own watermark trips — see `SAI_RX`). Refills one frame (L+R, 32-bit
/// left-justified) per pass until the request flag clears, rendering a fresh
/// block from the engine at each block boundary. Also drains and discards
/// whatever RX collected, every pass, unconditionally — see `SAI_RX` for why
/// that isn't optional.
#[no_mangle]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn SAI1() {
    // SAFETY: this is the only context that touches these statics; setup ran
    // before the interrupt was unmasked, and the main loop's schedule_midi is
    // interrupt::free'd.
    let tx = unsafe { &mut *SAI_TX.as_mut_ptr() };
    let rx = unsafe { &mut *SAI_RX.as_mut_ptr() };
    let st = unsafe { &mut *addr_of_mut!(AUDIO) };
    let led = LED.load(Ordering::Relaxed);

    // Drain RX first and unconditionally: we don't want its data, only to
    // keep its FIFO from filling and backpressuring the shared bit clock.
    // (An experiment disabling this to test its per-ISR overhead against the
    // aliasing symptom made no difference — ruled out, restored.)
    let mut discard = [0u32; 2];
    while rx.status().contains(Status::FIFO_REQUEST) {
        rx.read_frame_u32(0, &mut discard);
    }

    while tx.status().contains(Status::FIFO_REQUEST) {
        if st.block_pos == BLOCK {
            if !TEST_TONE {
                // SAFETY: ENGINE was stored by setup before this ISR was
                // unmasked, and the main loop cannot touch the engine while
                // this ISR runs (interrupt::free) or concurrently (no other
                // ISR).
                let engine = unsafe { &mut *ENGINE.load(Ordering::Acquire) };
                engine.process(&mut st.left, &mut st.right);
            }
            st.block_pos = 0;
            SAMPLE_COUNTER.fetch_add(BLOCK as u32, Ordering::Relaxed);
        }

        let (sample_l, sample_r) = if TEST_TONE {
            // 440 Hz sine — the same `SineOsc`/`sin_turns` the engine's own
            // sine-based machines use, at half amplitude.
            let level = st.tone_osc.tick() * 0.5;
            (level, level)
        } else {
            (st.left[st.block_pos], st.right[st.block_pos])
        };

        // Blink the on-board LED at ~1 Hz from inside the ISR. A solid LED
        // means the ISR is firing but spinning (bit clock too fast to keep
        // the FIFO fed); a dark LED means it never fires at all.
        st.led_counter += 1;
        if st.led_counter >= SAMPLE_RATE_HZ {
            st.led_counter = 0;
            if !led.is_null() {
                // SAFETY: `start` handed us a pointer to a `main`-lifetime
                // LED, and `toggle` is an atomic hardware write.
                unsafe { &*led }.toggle();
            }
        }

        // f32 in [-1,1] → signed i16 (round to nearest, then saturating
        // cast), reinterpreted as a u16 bit pattern, then left-justified
        // into the 32-bit slot.
        //
        // `as i16` truncates toward zero, not round-to-nearest — harmless
        // for a loud signal, but a decaying envelope's tail spends a lot of
        // its time below one LSB (1/32768 ≈ 0.00003), and truncation turns
        // that into a correlated staircase rather than the much less
        // audible symmetric error rounding gives. The host's WAV writer
        // keeps 24-bit precision throughout and never hits this at all —
        // real divergence, just downstream of everything captured/compared
        // so far, all of which was pre-quantization.
        // `f32::round()` needs `libm` (not available in `core`, and not
        // worth a new dependency for one call) — round-half-away-from-zero
        // by hand: add a half-step in the direction of the sign before the
        // truncating cast.
        let scaled_l = sample_l.clamp(-1.0, 1.0) * 32767.0;
        let scaled_r = sample_r.clamp(-1.0, 1.0) * 32767.0;
        let left = (scaled_l + if scaled_l >= 0.0 { 0.5 } else { -0.5 }) as i16;
        let right = (scaled_r + if scaled_r >= 0.0 { 0.5 } else { -0.5 }) as i16;
        tx.write_frame_u32(
            0,
            &[
                (left as u16 as u32) << 16,
                (right as u16 as u32) << 16,
            ],
        );
        st.block_pos += 1;
    }
}
