//! Novation Launch Control XL 3 as the device's front panel (#13).
//!
//! Twenty-four endless encoders and eight faders editing the selected
//! track, the parameter's name and value on the controller's OLED, and the
//! encoder LEDs coloured by what they currently do. Two-way, and that is
//! the point: the driver owns a UI state machine, the surface is a display
//! as much as an input, and every state change repaints it.
//!
//! This is a UX layer over the control plane #1 landed. No engine DSP
//! changes, no audio change, no change to the NRPN wire contract — the
//! driver emits [`MidiEvent`](drum_engine::midi::MidiEvent)s and they go
//! through the same `handle_midi` router the Teensy runs.
//!
//! # Layout
//!
//! | Module | What it is | Phase 3 |
//! |--------|------------|---------|
//! | [`surface`] | The UI state machine. No I/O, no `alloc`, no `std`. | moves to `core` |
//! | [`wire`] | DAW-mode constants, SysEx and CC codecs, the decoder. | stays host-side, informs the firmware's |
//! | [`driver`] | midir ports, startup/shutdown, pacing, repaint. | rewritten over USB host |
//!
//! The split is the point. Phase 3 of #13 takes the same driver to the
//! Teensy over USB host, and the part worth protecting across that move is
//! the screen and modal logic, not the byte formatting — so the state
//! machine is kept I/O-free from the start and is tested without any
//! hardware at all.
//!
//! # Where it runs
//!
//! DAW mode is reachable on the controller's DAW In/Out USB port, so this
//! starts on the host rig: `render lcxl3`, which is `render device` — the
//! firmware's main loop relocated to macOS — with the surface attached.

pub mod driver;
pub mod probe;
pub mod surface;
pub mod wire;

use drum_engine::{DeviceModel, SlotId};

pub use driver::{Config, Driver};
pub use surface::{Catalogue, Surface};

/// Build a [`Catalogue`] for a device from its machine identifier type.
///
/// Everything the surface needs about a device is already on `DeviceModel`
/// and `SlotId`; this is the adapter, and the reason the surface itself is
/// not generic over an engine.
///
/// An out-of-range catalogue index cannot arise from the surface — it
/// quantises onto `count` before looking anything up — but the lookups are
/// total anyway, falling back to machine 0, because a panic on a control
/// surface takes the audio thread's host process with it.
pub fn catalogue_for<Id, const N: usize>() -> Catalogue<N>
where
    Id: DeviceModel<N> + 'static,
{
    fn resolve<Id: SlotId<N>, const N: usize>(i: usize) -> Id {
        Id::from_index(i)
            .or_else(|| Id::from_index(0))
            .expect("a device catalogue has at least one machine")
    }
    Catalogue {
        count: Id::count(),
        table: |i| resolve::<Id, N>(i).param_table().as_slice(),
        defaults: |i| resolve::<Id, N>(i).default_params(),
        label: |i| resolve::<Id, N>(i).label(),
    }
}

/// Check the controller is there before anything expensive happens.
///
/// Without this, a mistyped `--surface-port` opens the audio device,
/// prints the engine's banner and *then* fails — which reads as "the rig
/// is up" right up until it is not.
pub fn preflight(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let filter = config.port.as_deref();
    let out = midir::MidiOutput::new("drumsynth lcxl3 preflight")?;
    let out_names: Vec<String> = out
        .ports()
        .iter()
        .map(|p| out.port_name(p).unwrap_or_else(|_| "?".into()))
        .collect();
    let input = midir::MidiInput::new("drumsynth lcxl3 preflight")?;
    let in_names: Vec<String> = input
        .ports()
        .iter()
        .map(|p| input.port_name(p).unwrap_or_else(|_| "?".into()))
        .collect();

    let needle = driver::describe_filter(filter);
    for (kind, found, all) in [
        ("output", driver::pick_port(&out_names, filter), &out_names),
        ("input", driver::pick_port(&in_names, filter), &in_names),
    ] {
        if found.is_none() {
            // One line on purpose: `main` reports a `Box<dyn Error>`
            // through `Debug`, which escapes newlines into the message.
            return Err(format!(
                "no MIDI {kind} port matching {needle} — connect the controller; DAW mode lives on its DAW In/Out USB port. {kind} ports seen: {}",
                if all.is_empty() {
                    "none at all".to_string()
                } else {
                    all.join(", ")
                }
            )
            .into());
        }
    }
    Ok(())
}

/// Read a device's current per-track state, for seeding a [`Surface`].
///
/// Must run *before* the engine moves to the audio thread. The shadow
/// models `Track::param_target` — the value a relative NRPN step is taken
/// from — so that is what is read here, rather than the live smoothed
/// value.
pub fn snapshot<E, const N: usize>(engine: &E) -> Vec<(usize, [f32; N])>
where
    E: drum_engine::DeviceEngine<N>,
    <E::Slot as drum_engine::Slot<N>>::Id: DeviceModel<N>,
{
    engine
        .tracks()
        .iter()
        .map(|t| {
            let mut params = [0.0f32; N];
            for (i, v) in params.iter_mut().enumerate() {
                *v = t.param_target(i);
            }
            (SlotId::index(t.id()), params)
        })
        .collect()
}

/// Run the surface against an already-running engine.
///
/// Blocks until ctrl-c or an audio-stream error, then takes the controller
/// back out of DAW mode. `events` is the ring the audio callback drains
/// between blocks — the surface never touches the engine directly, exactly
/// as the CoreMIDI path never does.
///
/// Inputs arrive on a midir callback thread and are forwarded over a
/// channel rather than handled there: the surface owns a shadow of every
/// parameter and the painted state of the controller, and one owner on one
/// thread is simpler than a mutex around both.
pub fn run_surface<Id, const N: usize>(
    config: &Config,
    kit: &[(usize, [f32; N])],
    mut events: rtrb::Producer<drum_engine::midi::MidiEvent>,
    errors: &std::sync::mpsc::Receiver<cpal::StreamError>,
) -> Result<(), Box<dyn std::error::Error>>
where
    Id: DeviceModel<N> + 'static,
{
    use std::sync::atomic::Ordering;

    let mut surface = Surface::<N>::new(catalogue_for::<Id, N>(), kit);
    surface.set_detent_lsb(config.detent_lsb);
    let mut driver = Driver::start(surface, config)?;

    let (tx, rx) = std::sync::mpsc::channel();
    let _input = driver::open_input(config.port.as_deref(), config.trace, move |msg| {
        let _ = tx.send(msg);
    })?;

    println!();
    println!("Launch Control XL 3 in DAW mode.");
    println!("  top buttons     select a track and audition it (shift: select only)");
    println!("  bottom buttons  hold for that group's parameters, tap to latch;");
    println!("                  tap the latched one again to return to the macros");
    println!("  encoders        the parameters on screen, for the selected track");
    println!("  faders          track levels, always");
    println!("  page up/down    the rest of the group");
    println!("ctrl-c to stop and return the controller to standalone mode");
    println!();

    let running = driver::interrupt_flag();
    let mut dropped = 0usize;
    while running.load(Ordering::Relaxed) {
        if let Ok(e) = errors.try_recv() {
            driver.shutdown();
            eprintln!("stream error: {e}");
            return Err("audio stream failed".into());
        }
        // Drain whatever the controller sent, then repaint once. Repainting
        // per message would send a burst mid-gesture and the budget would
        // throw most of it away.
        while let Ok(msg) = rx.try_recv() {
            if !driver.on_incoming(msg, &mut events) {
                dropped += 1;
            }
        }
        driver.render();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    driver.shutdown();
    if dropped > 0 {
        eprintln!("warning: {dropped} surface event(s) dropped — engine queue full");
    }
    println!("\nleft DAW mode");
    Ok(())
}
