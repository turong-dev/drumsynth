//! The firmware's USB stack.
//!
//! The Teensy is a single-role USB *device*: everything that talks to the
//! host on the one USB connector has to share a single
//! [`UsbDevice`](usb_device::device::UsbDevice). Today that is MIDI in (the
//! groove box → pads → engine path); later it is a CDC serial port so the
//! `screen /dev/ttyACM0` logging workflow promised by the crate docs
//! survives without imxrt-log's separate stack.
//!
//! Two off-the-shelf crates cannot provide this, which is why the MIDI
//! class lives here:
//!
//! - `imxrt-log`'s `usbd` backend builds *and owns* its entire USB stack in
//!   its own statics, so it cannot host a second class on the same bus.
//! - `usbd-midi` 0.2.0 — the newest version built on usb-device 0.2, which
//!   `imxrt-usbd` 0.2.2 (and therefore everything USB on imxrt-hal 0.5) is
//!   pinned to — only implements the host→device direction and its
//!   descriptors are wrong in places. 0.5.1 is correct but needs usb-device
//!   0.3, which that stack does not and cannot provide.
//!
//! So [`MidiClass`] is ~150 lines of descriptors written against the USB
//! MIDI 1.0 spec — the same layout `usbd-midi` 0.5.1 emits — on top of the
//! `BusAdapter` that `imxrt-log` proves works on this silicon. The static
//! construction and poll loop mirror `imxrt-log`'s `usbd.rs` exactly, so the
//! two implementations stay directly comparable.

use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use teensy4_bsp as bsp;

use bsp::usbd::{BusAdapter, EndpointMemory, EndpointState, Instances, Speed};
use usb_device::bus::{UsbBus, UsbBusAllocator};
use usb_device::class_prelude::{
    DescriptorWriter, EndpointIn, EndpointOut, InterfaceNumber, UsbClass,
};
use usb_device::device::{
    StringDescriptors, UsbDevice, UsbDeviceBuilder, UsbDeviceState, UsbVidPid,
};
use usb_device::endpoint::EndpointAddress;
use usb_device::{LangID, Result, UsbDirection};

// ---- static USB objects ---------------------------------------------------

// Two 64-byte bulk endpoints + EP0 control + slack. This is deliberately
// sized for the MIDI class only. If a CDC class joins this bus its bulk
// endpoints are 512 bytes and the buffer must grow to ~1280 (see
// imxrt-log's `usbd.rs`, which documents the panic you will get otherwise).
const EP_MEMORY_BYTES: usize = 64 * 2 + 64 * 2 + 128;
static EP_MEMORY: EndpointMemory<EP_MEMORY_BYTES> = EndpointMemory::new();

// Endpoints are counted in pairs (OUT + IN per index). 8 = EP0 control +
// three pairs of data endpoints, one of them spare for a future CDC class.
static EP_STATE: EndpointState<8> = EndpointState::new();

type Bus = BusAdapter;
type BusAllocator = UsbBusAllocator<Bus>;
type Device<'a> = UsbDevice<'a, Bus>;

static mut BUS: MaybeUninit<BusAllocator> = MaybeUninit::uninit();
static mut DEVICE: MaybeUninit<Device<'static>> = MaybeUninit::uninit();
static mut MIDI: MaybeUninit<MidiClass<'static, Bus>> = MaybeUninit::uninit();

// Whether the device has made the not-configured → configured transition
// since the last poll, i.e. whether `bus.configure()` is owed.
static CONFIGURED: AtomicBool = AtomicBool::new(false);

// pid.codes shared VID. The PID is free to claim on pid.codes before this
// ever ships; 0x0001 is the "I am a random prototype" value.
const VID_PID: UsbVidPid = UsbVidPid(0x1209, 0x0001);
const PRODUCT: &str = "drumkit";

// ---- USB MIDI 1.0 (Audio 1.0 class) descriptor constants -----------------

const USB_AUDIO_CLASS: u8 = 0x01;
const USB_AUDIOCONTROL_SUBCLASS: u8 = 0x01;
const USB_MIDISTREAMING_SUBCLASS: u8 = 0x03;
const CS_INTERFACE: u8 = 0x24;
const CS_ENDPOINT: u8 = 0x25;
const HEADER_SUBTYPE: u8 = 0x01;
const MIDI_IN_JACK_SUBTYPE: u8 = 0x02;
const MIDI_OUT_JACK_SUBTYPE: u8 = 0x03;
const MS_HEADER_SUBTYPE: u8 = 0x01;
const MS_GENERAL: u8 = 0x01;
const EMBEDDED: u8 = 0x01;
const EXTERNAL: u8 = 0x02;

/// Bulk max packet size for the MIDI endpoints.
///
/// MIDI carries 4-byte event packets; 64 keeps latency low and is what every
/// other MIDI class uses.
const MAX_PACKET_SIZE: u16 = 64;

/// A USB MIDI 1.0 device class.
///
/// Two bulk endpoints, 64 bytes each: OUT carries 4-byte USB MIDI event
/// packets *from* the host (this is how the groove box plays the engine);
/// IN is allocated but unused for now, kept for a future note echo /
/// DAW-monitoring path. One embedded jack per direction — the minimum a
/// compliant class can advertise.
pub struct MidiClass<'a, B: UsbBus> {
    ac_interface: InterfaceNumber,
    ms_interface: InterfaceNumber,
    bulk_out: EndpointOut<'a, B>,
    bulk_in: EndpointIn<'a, B>,
}

impl<'a, B: UsbBus> MidiClass<'a, B> {
    fn new(alloc: &'a UsbBusAllocator<B>) -> Self {
        Self {
            ac_interface: alloc.interface(),
            ms_interface: alloc.interface(),
            bulk_out: alloc.bulk(MAX_PACKET_SIZE),
            bulk_in: alloc.bulk(MAX_PACKET_SIZE),
        }
    }

    /// Read host bytes into `buffer`.
    ///
    /// Host data arrives in 4-byte USB MIDI event packets; `Ok(n)` means
    /// `buffer[..n]` holds whole packets. `Ok(0)` means nothing new (or a
    /// zero-length transfer). Callers parse the bytes with a
    /// [`MidiParser`](drum_engine::midi::MidiParser) exactly as if they had
    /// come off a DIN socket — the USB MIDI transport is just a wrapper
    /// around ordinary MIDI bytes.
    pub fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        match self.bulk_out.read(buffer) {
            Ok(n) => Ok(n),
            Err(usb_device::UsbError::WouldBlock) => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Write one 4-byte USB MIDI event packet to the host.
    ///
    /// Returns `Ok(4)` when the packet was accepted, `Ok(0)` when the
    /// endpoint is busy (`WouldBlock`), or the underlying error otherwise.
    pub fn write(&mut self, packet: &[u8; 4]) -> Result<usize> {
        match self.bulk_in.write(packet) {
            Ok(n) => Ok(n),
            Err(usb_device::UsbError::WouldBlock) => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl<B: UsbBus> UsbClass<B> for MidiClass<'_, B> {
    fn get_configuration_descriptors(&self, writer: &mut DescriptorWriter) -> Result<()> {
        // One in jack + one out jack, so the class-specific streaming block
        // is the fixed 65 bytes below (the usbd-midi 0.5.1 formula:
        // 7 + jacks·(6+9) + (9+4+1)·2 = 7 + 30 + 28).
        const N_IN_JACKS: u8 = 1;
        const N_OUT_JACKS: u8 = 1;
        const MS_TOTAL_LENGTH: u16 = 7
            + (N_IN_JACKS + N_OUT_JACKS) as u16 * (6 + 9)
            + (9 + 4 + N_OUT_JACKS as u16)
            + (9 + 4 + N_IN_JACKS as u16);
        // Compile-time check that the class-specific streaming block really
        // is 65 bytes for the 1-in/1-out case, so the descriptor sizes above
        // can never drift out of sync with the header we publish.
        const _: () = assert!(MS_TOTAL_LENGTH == 65);

        // --- AudioControl interface (the header for the MIDI streaming
        // --- interface that follows; no endpoints of its own) ---
        writer.interface(
            self.ac_interface,
            USB_AUDIO_CLASS,
            USB_AUDIOCONTROL_SUBCLASS,
            0,
        )?;
        writer.write(
            CS_INTERFACE,
            &[
                HEADER_SUBTYPE,
                0x00,
                0x01, // Audio 1.0 revision
                0x09,
                0x00, // class-specific descriptor length
                0x01, // one streaming interface
                0x01, // ...which is the MIDI streaming interface below
            ],
        )?;

        // --- MIDIStreaming interface ---
        writer.interface(
            self.ms_interface,
            USB_AUDIO_CLASS,
            USB_MIDISTREAMING_SUBCLASS,
            0,
        )?;
        let ms_start = writer.position();
        writer.write(
            CS_INTERFACE,
            &[
                MS_HEADER_SUBTYPE,
                0x00,
                0x01, // MIDI 1.0 revision
                (MS_TOTAL_LENGTH & 0xFF) as u8,
                (MS_TOTAL_LENGTH >> 8) as u8,
            ],
        )?;

        // Jacks, numbered exactly as usbd-midi 0.5.1 numbers them (1 in +
        // 1 out): external in = 1, embedded out = 2, external out = 3,
        // embedded in = 4. The bulk OUT endpoint associates with the
        // embedded *in* jack and bulk IN with the embedded *out* jack; that
        // pairing is what Linux's snd-usb-midi and macOS expect, and
        // copying the reference implementation byte-for-byte is the only
        // sensible move here.
        //
        // Topology: host → external in (1) → embedded out (2) → engine,
        // and (future note echo) engine → embedded in (4) → external
        // out (3) → host.
        writer.write(
            CS_INTERFACE,
            &[
                MIDI_IN_JACK_SUBTYPE,
                EXTERNAL,
                0x01, // id
                0x00,
            ],
        )?;
        writer.write(
            CS_INTERFACE,
            &[
                MIDI_IN_JACK_SUBTYPE,
                EMBEDDED,
                0x04, // id
                0x00,
            ],
        )?;
        writer.write(
            CS_INTERFACE,
            &[
                MIDI_OUT_JACK_SUBTYPE,
                EXTERNAL,
                0x03, // id
                0x01, // one source pin
                0x04, // ...from embedded in jack
                0x01, // ...its first pin
                0x00,
            ],
        )?;
        writer.write(
            CS_INTERFACE,
            &[
                MIDI_OUT_JACK_SUBTYPE,
                EMBEDDED,
                0x02, // id
                0x01, // one source pin
                0x01, // ...from external in jack
                0x01, // ...its first pin
                0x00,
            ],
        )?;

        // Bulk OUT endpoint (host → us), with its class-specific general
        // descriptor naming the embedded *in* jack (4) it feeds.
        writer.endpoint_ex(&self.bulk_out, |data| {
            data[0] = 0; // bRefresh
            data[1] = 0; // bSynchAddress
            Ok(2)
        })?;
        writer.write(
            CS_ENDPOINT,
            &[MS_GENERAL, N_OUT_JACKS, 0x04], // embedded in jack 4
        )?;

        // Bulk IN endpoint (us → host), naming the embedded *out* jack (2).
        writer.endpoint_ex(&self.bulk_in, |data| {
            data[0] = 0; // bRefresh
            data[1] = 0; // bSynchAddress
            Ok(2)
        })?;
        writer.write(
            CS_ENDPOINT,
            &[MS_GENERAL, N_IN_JACKS, 0x02], // embedded out jack 2
        )?;

        debug_assert_eq!(writer.position() - ms_start, MS_TOTAL_LENGTH as usize);
        Ok(())
    }
}

/// Bring up the USB bus and its MIDI class.
///
/// # Safety
///
/// Call exactly once, from `main`, before `poll` is ever called and before
/// interrupts are enabled. All the module statics are written here.
///
/// The mutable-static accesses are safe for the reasons documented at each
/// one; the `static_mut_refs` lint cannot see that they are exclusive and
/// single-shot.
#[allow(unsafe_code, static_mut_refs)]
pub unsafe fn init(usb: Instances<1>) {
    let bus = {
        // Safety: `BUS` is written exactly once here; `poll()` cannot run
        // before init returns. Single-threaded, interrupts disabled.
        let bus = unsafe {
            BusAdapter::without_critical_sections(usb, &EP_MEMORY, &EP_STATE, Speed::High)
        };
        bus.set_interrupts(false);
        let alloc = UsbBusAllocator::new(bus);
        // Safety: mutable static write, once.
        unsafe { BUS.write(alloc) }
    };

    {
        let midi = MidiClass::new(bus);
        // Safety: mutable static write, once.
        unsafe { MIDI.write(midi) };
    }

    {
        let device = UsbDeviceBuilder::new(bus, VID_PID)
            .strings(&[StringDescriptors::new(LangID::EN_US)
                .manufacturer("drumkit")
                .product(PRODUCT)])
            .expect("too many string descriptor languages")
            .max_packet_size_0(64)
            .expect("EP0 max packet size must be 8, 16, 32 or 64")
            .build();

        // The host may send a bulk transfer that is an exact multiple of the
        // max packet size; the hardware terminates those with a zero-length
        // packet so the host knows the transfer ended. Enable that for every
        // endpoint — ZLT is a no-op on endpoints we never allocated.
        for idx in 1..8 {
            for dir in &[UsbDirection::In, UsbDirection::Out] {
                let ep_addr = EndpointAddress::from_parts(idx, *dir);
                device.bus().enable_zlt(ep_addr);
            }
        }

        // Safety: mutable static write, once.
        unsafe { DEVICE.write(device) };
    }
}

/// Drive the USB bus and drain any MIDI the host has sent.
///
/// Call once per main-loop iteration. Returns the number of raw MIDI bytes
/// written into `dst` (a multiple of 4; each 4-byte group is one USB MIDI
/// event packet), or 0 when the device is unconfigured or the host is quiet.
///
/// # Safety
///
/// Only call from the same single execution context that called
/// [`init`], after `init` has returned. `poll` is not reentrant.
#[allow(unsafe_code, static_mut_refs)]
pub fn poll(dst: &mut [u8]) -> usize {
    // Safety: init has run; poll is called from one context, never
    // reentrantly, so the mutable statics are uniquely borrowed here.
    let device = unsafe { DEVICE.assume_init_mut() };
    let midi = unsafe { MIDI.assume_init_mut() };

    // Drives enumeration and control transfers, and dispatches class
    // callbacks. Returns whether there was class traffic, which we do not
    // need — MIDI is polled explicitly below.
    device.poll(&mut [midi]);

    if device.state() != UsbDeviceState::Configured {
        CONFIGURED.store(false, Ordering::Relaxed);
        return 0;
    }

    // Newly configured? The bus adapter needs an explicit kick, and on
    // every reconfiguration after a host reset/unplug.
    if !CONFIGURED.swap(true, Ordering::Relaxed) {
        device.bus().configure();
    }

    // Drain whatever the host has queued. `read` returns Ok(0) when there is
    // nothing, so this loops until the pipe is dry (or `dst` is full).
    let mut total = 0;
    while total < dst.len() {
        match midi.read(&mut dst[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => break,
        }
    }
    total
}

/// Send one 4-byte USB MIDI event packet to the host.
///
/// Returns `true` when the packet was accepted. If the bulk IN endpoint is
/// currently busy, the packet is dropped and `false` is returned. Callers
/// can rely on the next render pass to retry changed LEDs.
///
/// # Safety
///
/// Only call from the same single execution context that called [`init`],
/// after `init` has returned. Not reentrant.
#[allow(unsafe_code, static_mut_refs)]
pub fn send_midi(packet: &[u8; 4]) -> bool {
    // Safety: init has run; this is called from one context, never
    // reentrantly, so the mutable statics are uniquely borrowed here.
    let device = unsafe { DEVICE.assume_init_mut() };
    let midi = unsafe { MIDI.assume_init_mut() };

    if device.state() != UsbDeviceState::Configured {
        return false;
    }

    match midi.write(packet) {
        Ok(4) => true,
        _ => false,
    }
}
