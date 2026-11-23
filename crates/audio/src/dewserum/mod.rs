//! DewSerum: a two-oscillator wavetable synth in the style of Xfer Serum, built into Honeydew in pure Rust (it runs on
//! the desktop, Android and the web alike).
//!
//! Signal path of one voice: Osc A and Osc B (each a wavetable morphed by `WT Pos`, with up to seven unison voices)
//! → a multimode filter (low, high or band pass, on [`Biquad`]) → Env 1 (the amp) → pan. Env 2 and the LFO have no
//! place of their own in the path: they are sources of the four-slot modulation matrix. See [`wavetable`] for the
//! tables and [`modulation`] for the envelopes, the curve LFO and the matrix.
//!
//! Params are normalized (0..=1), by index, as for every instrument; [`param`] names the indices and [`defaults`] the
//! starting values. The instrument keeps the raw params and, at the next control tick after one changes, derives its
//! working values from them (a `Control`), so a param write only sets a flag. The LFO curve's params are handled
//! apart: a write marks the curve dirty and the next tick resamples it into the [`LfoTable`] in place. Nothing in the
//! audio path allocates or locks.
//!
//! Rendering runs in control ticks of [`TICK`] frames. Once per tick each voice steps Env 2 and the LFO (smoothed by
//! one pole, so a stepped curve does not zipper), evaluates the matrix and sets its oscillators' increments, wavetable
//! readers, gains and filter coefficients; per frame it runs the oscillators, the filter and Env 1, with the
//! oscillator gains ramped across the tick.

pub mod modulation;
pub mod wavetable;

use crate::ProcessContext;
use crate::biquad::{Biquad, Response};
use crate::instrument::Instrument;

use modulation::{
    AdsrParams, CurvePreset, Envelope, Lfo, LfoCurve, LfoSettings, LfoTable, ModDest,
    ModInputs, ModMatrix, SLOT_PARAMS, SLOTS, choice,
};
use wavetable::{Morph, Reader, WavetableBank, bank};

/// Frames per control tick.
pub const TICK: usize = 32;
/// Voices a note can use (the Voices param limits it from 1 to this).
pub const MAX_VOICES: usize = 8;
/// Unison voices of one oscillator.
pub const MAX_UNISON: usize = 7;

/// How far the outermost unison voices go at Detune 1, in semitones either way; the law is Detune².
const MAX_DETUNE_SEMITONES: f32 = 1.0;
/// The time constant of the LFO's smoothing.
const LFO_SMOOTH_SECONDS: f32 = 0.002;
/// Channel param 4, Pitch Bend: ±2 semitones (4v − 2).
const PITCH_BEND: usize = 4;
/// Channel param 8, MIDI CC #1: the mod wheel.
const MOD_WHEEL: usize = 8;

/// The param indices (param 0 is Enable, as for every module).
pub mod param {
    use super::modulation::{CURVE_PARAMS, SLOT_PARAMS, SLOTS};

    pub const ENABLE: usize = 0;
    /// Osc A's first param and Osc B's; each takes [`OSC_PARAMS`], in the order below.
    pub const OSC_BASE: [usize; 2] = [1, 9];
    pub const OSC_PARAMS: usize = 8;
    /// Offsets within an oscillator's params.
    pub const LEVEL: usize = 0;
    pub const WT_POS: usize = 1;
    /// Semitones, −24 … +24 in whole steps.
    pub const PITCH: usize = 2;
    /// ±100 cents.
    pub const FINE: usize = 3;
    /// 1 … 7 voices.
    pub const UNISON: usize = 4;
    pub const DETUNE: usize = 5;
    pub const BLEND: usize = 6;
    pub const WIDTH: usize = 7;

    /// Low pass, high pass, band pass.
    pub const FILTER_TYPE: usize = 17;
    pub const CUTOFF: usize = 18;
    pub const RESONANCE: usize = 19;
    pub const KEY_TRACK: usize = 20;
    /// Attack, Decay, Sustain, Release.
    pub const ENV1: usize = 21;
    pub const ENV2: usize = 25;
    pub const LFO_RATE: usize = 29;
    /// Hertz below one half, tempo-synced from it.
    pub const LFO_MODE: usize = 30;
    pub const LFO_BEAT: usize = 31;
    /// [`SLOTS`] slots of [`SLOT_PARAMS`]: source, destination, depth.
    pub const MATRIX: usize = 32;
    pub const MASTER: usize = 44;
    /// 1 … 8.
    pub const VOICES: usize = 45;
    /// The LFO curve's [`CURVE_PARAMS`] params ([`super::modulation::LfoCurve::to_params`]).
    pub const LFO_CURVE: usize = 46;
    pub const COUNT: usize = LFO_CURVE + CURVE_PARAMS;

    const _: () = assert!(MATRIX + SLOTS * SLOT_PARAMS == MASTER);
    const _: () = assert!(OSC_BASE[1] + OSC_PARAMS == FILTER_TYPE);
}

pub use param::COUNT as PARAM_COUNT;

/// The starting value of every param: a three-voice saw through a bright low pass, no modulation.
pub fn defaults() -> [f32; PARAM_COUNT] {
    use param::*;
    let mut p = [0.5; COUNT];
    p[ENABLE] = 1.0;
    // Level, WT Pos, Pitch, Fine, Unison, Detune, Blend, Width.
    let oscs = [
        [0.85, 2.0 / 3.0, 0.5, 0.5, 1.0 / 3.0, 0.35, 0.75, 0.6],
        [0.0, 0.0, 0.5, 0.5, 0.0, 0.25, 1.0, 0.5],
    ];
    for (base, values) in OSC_BASE.into_iter().zip(oscs) {
        p[base..base + OSC_PARAMS].copy_from_slice(&values);
    }
    p[FILTER_TYPE] = 0.0;
    p[CUTOFF] = 0.85;
    p[RESONANCE] = 0.15;
    p[KEY_TRACK] = 0.0;
    p[ENV1..ENV1 + 4].copy_from_slice(&[0.2, 0.5, 0.8, 0.4]);
    p[ENV2..ENV2 + 4].copy_from_slice(&[0.0, 0.5, 0.0, 0.5]);
    p[LFO_RATE] = 0.5;
    p[LFO_MODE] = 0.0;
    p[LFO_BEAT] = 0.5;
    for slot in 0..SLOTS {
        let at = MATRIX + slot * SLOT_PARAMS;
        p[at..at + SLOT_PARAMS].copy_from_slice(&[0.0, 0.0, 0.5]);
    }
    p[MASTER] = 0.7;
    p[VOICES] = 1.0;
    p[LFO_CURVE..].copy_from_slice(&CurvePreset::Triangle.curve().to_params());
    p
}

// ---------------------------------------------------------------------------------------------------------------------
// Param laws
// ---------------------------------------------------------------------------------------------------------------------

/// Pitch param: −24 … +24 semitones in whole steps.
fn semitones(v: f32) -> f32 {
    (v.clamp(0.0, 1.0) * 48.0).round() - 24.0
}

/// Where unison voice `k` of `n` sits: −1 … +1, 0 for a lone voice.
fn unison_position(n: usize, k: usize) -> f32 {
    if n <= 1 {
        0.0
    } else {
        2.0 * k as f32 / (n - 1) as f32 - 1.0
    }
}

/// The left and right gains of an oscillator's unison voices. Blend 1 weighs them all alike; towards 0 the voices
/// nearest the centre keep their weight and the outer ones fade, down to the central voice alone (with an even count
/// the two innermost, and with two voices Blend does nothing). The weights are power-normalized, so the stack's
/// loudness holds as Blend and the count change. Width spreads them over the stereo field, 0 all in the middle, with
/// the same pan law as the instrument's notes.
fn unison_weights(n: usize, blend: f32, width: f32) -> ([f32; MAX_UNISON], [f32; MAX_UNISON]) {
    let (blend, width) = (blend.clamp(0.0, 1.0), width.clamp(0.0, 1.0));
    let mut weight = [0.0f32; MAX_UNISON];
    for (k, w) in weight.iter_mut().enumerate().take(n) {
        *w = blend + (1.0 - blend) * (1.0 - unison_position(n, k).abs());
    }
    let top = weight[..n].iter().fold(0.0f32, |top, &w| top.max(w));
    for w in &mut weight[..n] {
        *w = if top > 1e-6 { *w / top } else { 1.0 };
    }
    let power: f32 = weight[..n].iter().map(|w| w * w).sum();
    let norm = 1.0 / power.sqrt();
    let (mut left, mut right) = ([0.0; MAX_UNISON], [0.0; MAX_UNISON]);
    for k in 0..n {
        let pan = 0.5 + 0.5 * width * unison_position(n, k);
        left[k] = weight[k] * norm * (2.0 - 2.0 * pan).min(1.0);
        right[k] = weight[k] * norm * (2.0 * pan).min(1.0);
    }
    (left, right)
}

// ---------------------------------------------------------------------------------------------------------------------
// Control: what the params mean, derived once after they change
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct OscControl {
    level: f32,
    wt_pos: f32,
    /// Pitch + Fine, in semitones.
    semitones: f32,
    unison: usize,
    detune: f32,
    left: [f32; MAX_UNISON],
    right: [f32; MAX_UNISON],
}

#[derive(Clone, Copy)]
struct Control {
    osc: [OscControl; 2],
    /// Env 1 runs per frame, Env 2 per tick.
    env1: AdsrParams,
    env2: AdsrParams,
    matrix: ModMatrix,
    response: Response,
    /// log2 of the cutoff in Hertz.
    cutoff_oct: f32,
    resonance: f32,
    key_track: f32,
    max_cutoff: f32,
    /// The one-pole coefficient of the LFO smoothing, per tick.
    lfo_coef: f32,
    /// The master gain (the Master param squared).
    master: f32,
    voice_limit: usize,
    /// Pitch Bend in semitones and the mod wheel, set by the controllers.
    bend: f32,
    wheel: f32,
}

impl Control {
    fn build(p: &[f32; PARAM_COUNT], sample_rate: f32, bend: f32, wheel: f32) -> Self {
        let osc = std::array::from_fn(|o| {
            let at = param::OSC_BASE[o];
            let unison = 1 + choice(p[at + param::UNISON], MAX_UNISON);
            let (left, right) = unison_weights(unison, p[at + param::BLEND], p[at + param::WIDTH]);
            OscControl {
                level: p[at + param::LEVEL],
                wt_pos: p[at + param::WT_POS],
                semitones: semitones(p[at + param::PITCH]) + (2.0 * p[at + param::FINE] - 1.0),
                unison,
                detune: p[at + param::DETUNE],
                left,
                right,
            }
        });
        let adsr = |at: usize, rate: f32| AdsrParams::new(p[at], p[at + 1], p[at + 2], p[at + 3], rate);
        let master = p[param::MASTER];
        Self {
            osc,
            env1: adsr(param::ENV1, sample_rate),
            env2: adsr(param::ENV2, sample_rate / TICK as f32),
            matrix: ModMatrix::from_params(
                &p[param::MATRIX..param::MATRIX + SLOTS * SLOT_PARAMS],
            ),
            response: match choice(p[param::FILTER_TYPE], 3) {
                0 => Response::LowPass,
                1 => Response::HighPass,
                _ => Response::BandPass,
            },
            // 20 Hz at 0, ten octaves up at 1.
            cutoff_oct: 20f32.log2() + 10.0 * p[param::CUTOFF],
            resonance: p[param::RESONANCE],
            key_track: p[param::KEY_TRACK],
            max_cutoff: (0.45 * sample_rate).clamp(20.0, 20_000.0),
            lfo_coef: 1.0 - (-(TICK as f32) / (sample_rate * LFO_SMOOTH_SECONDS)).exp(),
            master: master * master,
            voice_limit: 1 + choice(p[param::VOICES], MAX_VOICES),
            bend,
            wheel,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------------------------------------------------

const WT_DEST: [ModDest; 2] = [ModDest::OscAWtPos, ModDest::OscBWtPos];
const PITCH_DEST: [ModDest; 2] = [ModDest::OscAPitch, ModDest::OscBPitch];
const LEVEL_DEST: [ModDest; 2] = [ModDest::OscALevel, ModDest::OscBLevel];
const DETUNE_DEST: [ModDest; 2] = [ModDest::OscADetune, ModDest::OscBDetune];

/// xorshift32: the random start phases of the unison voices, the same on every run.
struct Rng(u32);

impl Rng {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

struct Voice {
    note: u8,
    velocity: f32,
    /// The note's pan, 0..=1.
    pan: f32,
    /// Slide offsets: semitones, pan, velocity.
    slide: [f32; 3],
    /// Orders the voices for stealing: the note-on count when it started.
    age: u64,
    env1: Envelope,
    env2: Envelope,
    lfo: Lfo,
    /// The LFO after its smoothing.
    lfo_smooth: f32,
    /// Left and right.
    filter: [Biquad; 2],
    phase: [[u32; MAX_UNISON]; 2],
    inc: [[u32; MAX_UNISON]; 2],
    readers: [[Reader<'static>; MAX_UNISON]; 2],
    /// Each oscillator's gain now and its step per frame towards this tick's target.
    gain: [f32; 2],
    gain_step: [f32; 2],
    vel_gain: f32,
    pan_gain: [f32; 2],
    /// Not yet updated since it started: the first update sets its gains and LFO outright, with no ramp.
    fresh: bool,
}

impl Voice {
    fn new() -> Self {
        let idle = bank().reader(0, Morph::new(0.0));
        Self {
            note: 0,
            velocity: 0.0,
            pan: 0.5,
            slide: [0.0; 3],
            age: 0,
            env1: Envelope::default(),
            env2: Envelope::default(),
            lfo: Lfo::default(),
            lfo_smooth: 0.0,
            filter: [Biquad::default(); 2],
            phase: [[0; MAX_UNISON]; 2],
            inc: [[0; MAX_UNISON]; 2],
            readers: [[idle; MAX_UNISON]; 2],
            gain: [0.0; 2],
            gain_step: [0.0; 2],
            vel_gain: 0.0,
            pan_gain: [1.0; 2],
            fresh: true,
        }
    }

    /// Starts a note on this voice. A voice taken while it still sounds keeps its phases and Env 1 level, so the steal
    /// does not click; an idle one starts clean, its first unison voice at phase 0 and the others at random.
    fn start(&mut self, note: u8, velocity: f32, pan: f32, age: u64, lfo_phase: f64, rng: &mut Rng) {
        if self.env1.is_idle() {
            self.env1.reset();
            for f in &mut self.filter {
                f.reset();
            }
            for phases in &mut self.phase {
                for (k, phase) in phases.iter_mut().enumerate() {
                    *phase = if k == 0 { 0 } else { rng.next_u32() };
                }
            }
        }
        self.note = note;
        self.velocity = velocity;
        self.pan = pan;
        self.slide = [0.0; 3];
        self.age = age;
        self.env1.gate_on();
        self.env2.reset();
        self.env2.gate_on();
        self.lfo.start(lfo_phase);
        self.fresh = true;
    }

    fn release(&mut self) {
        if self.env1.is_held() {
            self.env1.gate_off();
            self.env2.gate_off();
        }
    }

    /// The per-tick work: Env 2 and the LFO step (`advance`), the matrix, then the oscillators' increments, readers
    /// and gain targets and the filter's coefficients.
    fn update(
        &mut self,
        c: &Control,
        table: &LfoTable,
        lfo: &LfoSettings,
        sample_rate: f32,
        advance: bool,
    ) {
        if advance {
            self.env2.advance(&c.env2);
            self.lfo.advance(lfo.step);
        }
        let raw = self.lfo.value(table);
        if self.fresh {
            self.lfo_smooth = raw;
        } else {
            self.lfo_smooth += (raw - self.lfo_smooth) * c.lfo_coef;
        }
        let pitch = f32::from(self.note) + self.slide[0] + c.bend;
        let inputs = ModInputs {
            lfo: self.lfo_smooth,
            env1: self.env1.level(),
            env2: self.env2.level(),
            velocity: self.velocity,
            key: ModInputs::key_value(pitch),
            wheel: c.wheel,
        };
        let m = c.matrix.evaluate(&inputs);

        let wt = bank();
        for o in 0..2 {
            let osc = &c.osc[o];
            let morph = Morph::new(osc.wt_pos + m.get(WT_DEST[o]));
            let note = (pitch + osc.semitones + m.get(PITCH_DEST[o])).clamp(-48.0, 160.0);
            let detune = (osc.detune + m.get(DETUNE_DEST[o])).clamp(0.0, 1.0);
            let spread = detune * detune * MAX_DETUNE_SEMITONES;
            for k in 0..osc.unison {
                let semitone = note + spread * unison_position(osc.unison, k);
                let freq = (440.0 * ((semitone - 69.0) / 12.0).exp2()).min(0.49 * sample_rate);
                self.inc[o][k] =
                    (f64::from(freq) / f64::from(sample_rate) * 4_294_967_296.0) as u32;
                self.readers[o][k] = wt.reader(WavetableBank::level_for(freq, sample_rate), morph);
            }
            let level = (osc.level + m.get(LEVEL_DEST[o])).clamp(0.0, 1.0);
            let target = level * level;
            if self.fresh {
                self.gain[o] = target;
                self.gain_step[o] = 0.0;
            } else {
                self.gain_step[o] = (target - self.gain[o]) / TICK as f32;
            }
        }

        let cutoff_oct = c.cutoff_oct
            + m.get(ModDest::Cutoff)
            + c.key_track * (pitch - 60.0) / 12.0;
        let cutoff = cutoff_oct.exp2().clamp(20.0, c.max_cutoff);
        let q = 0.5 * 20f32.powf((c.resonance + m.get(ModDest::Resonance)).clamp(0.0, 1.0));
        for f in &mut self.filter {
            f.set_filter_response(c.response, cutoff, q, sample_rate);
        }

        self.vel_gain = (self.velocity + self.slide[2]).clamp(0.0, 1.0);
        let pan = (self.pan + self.slide[1]).clamp(0.0, 1.0);
        self.pan_gain = [(2.0 - 2.0 * pan).min(1.0), (2.0 * pan).min(1.0)];
        self.fresh = false;
    }

    /// Renders `n` ≤ [`TICK`] frames, adding into `mix`.
    fn render(&mut self, n: usize, c: &Control, mix: &mut [[f32; TICK]; 2]) {
        for i in 0..n {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for o in 0..2 {
                let gain = self.gain[o];
                // An oscillator at gain 0 and staying there is skipped, phases and all.
                if gain != 0.0 || self.gain_step[o] != 0.0 {
                    let osc = &c.osc[o];
                    let (mut sum_l, mut sum_r) = (0.0f32, 0.0f32);
                    for k in 0..osc.unison {
                        let phase = self.phase[o][k].wrapping_add(self.inc[o][k]);
                        self.phase[o][k] = phase;
                        let s = self.readers[o][k].read(phase);
                        sum_l += s * osc.left[k];
                        sum_r += s * osc.right[k];
                    }
                    l += sum_l * gain;
                    r += sum_r * gain;
                }
                self.gain[o] = gain + self.gain_step[o];
            }
            let amp = self.env1.advance(&c.env1) * self.vel_gain;
            mix[0][i] += self.filter[0].process(l) * amp * self.pan_gain[0];
            mix[1][i] += self.filter[1].process(r) * amp * self.pan_gain[1];
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The instrument
// ---------------------------------------------------------------------------------------------------------------------

pub struct DewSerum {
    sample_rate: f32,
    params: [f32; PARAM_COUNT],
    control: Control,
    /// A param other than the curve's changed: `control` is stale.
    dirty: bool,
    lfo_table: LfoTable,
    /// A curve param changed: `lfo_table` is stale.
    curve_dirty: bool,
    lfo_settings: LfoSettings,
    voices: [Voice; MAX_VOICES],
    rng: Rng,
    note_count: u64,
    tempo_bpm: f32,
    /// The song position of the next frame, in beats.
    song_beat: f64,
    tick_pos: usize,
    master_now: f32,
    master_step: f32,
}

impl DewSerum {
    /// Builds the instrument (and, the first time, the wavetable bank) on the app thread.
    pub fn new(sample_rate: f32) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            sample_rate
        } else {
            48_000.0
        };
        // Builds the shared wavetable bank here, so the audio thread only ever finds it built.
        let _ = bank();
        let params = defaults();
        let control = Control::build(&params, sample_rate, 0.0, 0.0);
        let curve = LfoCurve::from_params(&params[param::LFO_CURVE..]);
        Self {
            sample_rate,
            params,
            master_now: control.master,
            control,
            dirty: false,
            lfo_table: LfoTable::from_curve(&curve),
            curve_dirty: false,
            lfo_settings: LfoSettings::new(
                params[param::LFO_RATE],
                params[param::LFO_MODE],
                params[param::LFO_BEAT],
                120.0,
                0.0,
                sample_rate,
                TICK,
            ),
            voices: std::array::from_fn(|_| Voice::new()),
            rn
