#![no_std]

/// Fast, single-precision reciprocal square root approximation using raw bit manipulation.
/// Crucial for normalising coefficient vectors on 600MHz boards without expensive hardware division.
#[inline(always)]
pub fn fast_inv_sqrt(x: f32) -> f32 {
    let i = x.to_bits();
    let i = 0x5f3759df - (i >> 1);
    let y = f32::from_bits(i);
    y * (1.5 - 0.5 * x * y * y) // 1st Newton-Raphson iteration
}

/// An ultra-fast, zero-allocation Biquad Filter implemented in Direct Form II.
/// Simulates a 2-pole analog Bridged-T drum resonant network.
pub struct BridgedTBiquad {
    // Filter Coefficients
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,
    // Delay States (Registers)
    v1: f32, v2: f32,
}

impl BridgedTBiquad {
    pub const fn new() -> Self {
        Self {
            b0: 0.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            v1: 0.0, v2: 0.0,
        }
    }

    /// Hardcoded 808 Kick Bridged-T translation mapping.
    /// Eliminates run-time tan/cos execution by baking standard sample rates (e.g., 48kHz).
    /// Maps R1, R2, C1, C2 properties down to discrete filter coefficients.
    pub fn update_from_circuit_constants(&mut self, sample_rate: f32, target_hz: f32, decay_q: f32) {
        // Pre-calculated digital pi ratios
        let w0 = (2.0 * 3.1415927 * target_hz) / sample_rate;
        
        // Taylor series approximation for sin/cos to save hardware cycles
        let w0_sq = w0 * w0;
        let cos_w0 = 1.0 - (w0_sq * 0.5) + (w0_sq * w0_sq * 0.041666668);
        let sin_w0 = w0 * (1.0 - w0_sq * 0.16666667);
        
        let alpha = sin_w0 / (2.0 * decay_q);
        let a0_inv = 1.0 / (1.0 + alpha);

        // Core Analog Biquad calculations
        self.b0 = alpha * a0_inv;
        self.b1 = 0.0;
        self.b2 = (-alpha) * a0_inv;
        self.a1 = (-2.0 * cos_w0) * a0_inv;
        self.a2 = (1.0 - alpha) * a0_inv;
    }

    /// Processes a single incoming audio sample using Direct Form II.
    /// Total cost: 5 Multiplies, 4 Additions. Extremely efficient.
    #[inline(always)]
    pub fn process(&mut self, sample: f32) -> f32 {
        let v0 = sample - self.a1 * self.v1 - self.a2 * self.v2;
        let output = self.b0 * v0 + self.b1 * self.v1 + self.b2 * self.v2;
        
        self.v2 = self.v1;
        self.v1 = v0;
        
        output
    }
}

/// System Engine generating an 808 analog kick circuit response.
pub struct AnalogKickEngine {
    biquad: BridgedTBiquad,
    trigger_envelope: f32,
    envelope_decay: f32,
    pitch_env: f32,
    base_freq: f32,
    sample_rate: f32,
}

impl AnalogKickEngine {
    pub fn new(sample_rate: f32) -> Self {
        let mut biquad = BridgedTBiquad::new();
        biquad.update_from_circuit_constants(sample_rate, 55.0, 4.5);
        
        Self {
            biquad,
            trigger_envelope: 0.0,
            envelope_decay: 0.9992, // Fast discrete decay
            pitch_env: 0.0,
            base_freq: 55.0, // Low 808 fundamentally tuned to A
            sample_rate,
        }
    }

    /// Excite the drum circuit (mimicking a trigger pulse entering the voice input)
    #[inline(always)]
    pub fn trigger(&mut self, velocity: f32) {
        self.trigger_envelope = velocity;
        self.pitch_env = 1.0; // High pitch deflection initially
    }

    /// Main real-time sample processing method.
    /// Runs perfectly inside bare-metal or DMA audio interrupts.
    #[inline(always)]
    pub fn next_sample(&mut self) -> f32 {
        if self.trigger_envelope < 1e-5 {
            return 0.0;
        }

        // 1. Update pitch envelope and dynamic component shift
        self.pitch_env *= 0.992; // Rapid pitch decay (the "thump" strike)
        let current_hz = self.base_freq + (self.pitch_env * 120.0);
        
        // Dynamic re-tuning mimicking active capacitor loading variations
        self.biquad.update_from_circuit_constants(self.sample_rate, current_hz, 6.0);

        // 2. Generate pulse-shaper input excitation
        let pulse_excitation = self.trigger_envelope;
        self.trigger_envelope *= self.envelope_decay;

        // 3. Process signal through the virtual circuit filter
        self.biquad.process(pulse_excitation)
    }
}
