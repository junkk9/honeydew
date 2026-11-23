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
            rng: Rng(0x9E37_79B9),
            note_count: 0,
            tempo_bpm: 120.0,
            song_beat: 0.0,
            tick_pos: 0,
            master_step: 0.0,
        }
    }

    /// Brings the derived state up to date with the params.
    fn refresh(&mut self) {
        if self.curve_dirty {
            let curve = LfoCurve::from_params(&self.params[param::LFO_CURVE..]);
            self.lfo_table.rebuild(&curve);
            self.curve_dirty = false;
        }
        if self.dirty {
            self.control = Control::build(
                &self.params,
                self.sample_rate,
                self.control.bend,
                self.control.wheel,
            );
            self.dirty = false;
        }
    }

    fn lfo_settings_now(&self) -> LfoSettings {
        LfoSettings::new(
            self.params[param::LFO_RATE],
            self.params[param::LFO_MODE],
            self.params[param::LFO_BEAT],
            self.tempo_bpm,
            self.song_beat,
            self.sample_rate,
            TICK,
        )
    }

    /// The control tick: refresh, then every sounding voice's per-tick update.
    fn tick(&mut self) {
        self.refresh();
        self.lfo_settings = self.lfo_settings_now();
        let (control, table, lfo, rate) = (
            &self.control,
            &self.lfo_table,
            &self.lfo_settings,
            self.sample_rate,
        );
        for voice in &mut self.voices {
            if !voice.env1.is_idle() {
                voice.update(control, table, lfo, rate, true);
            }
        }
        self.master_step = (self.control.master - self.master_now) / TICK as f32;
    }

    /// The voice a new note takes: an idle one among the first `limit`, else a released one, else the oldest.
    fn pick_voice(&self, limit: usize) -> usize {
        let limit = limit.clamp(1, MAX_VOICES);
        if let Some(free) = (0..limit).find(|&i| self.voices[i].env1.is_idle()) {
            return free;
        }
        (0..limit)
            .min_by_key(|&i| (self.voices[i].env1.is_held(), self.voices[i].age))
            .unwrap_or(0)
    }

    fn start_note(&mut self, key: u8, velocity: f32, pan: f32) {
        self.refresh();
        let lfo = self.lfo_settings_now();
        self.lfo_settings = lfo;
        let index = self.pick_voice(self.control.voice_limit);
        self.note_count += 1;
        let voice = &mut self.voices[index];
        voice.start(key, velocity, pan, self.note_count, lfo.start_phase, &mut self.rng);
        // Set up at once, so a note that starts between two ticks does not play with the last note's oscillators.
        voice.update(&self.control, &self.lfo_table, &lfo, self.sample_rate, false);
    }
}

impl Instrument for DewSerum {
    fn set_parameter(&mut self, index: usize, value: f32) {
        if index >= PARAM_COUNT || !value.is_finite() {
            return;
        }
        self.params[index] = value.clamp(0.0, 1.0);
        if index >= param::LFO_CURVE {
            self.curve_dirty = true;
        } else {
            self.dirty = true;
        }
    }

    fn note_on(&mut self, key: u8, velocity: f32, pan: f32) {
        let velocity = if velocity.is_finite() {
            velocity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let pan = if pan.is_finite() { pan.clamp(0.0, 1.0) } else { 0.5 };
        self.start_note(key.min(127), velocity, pan);
    }

    fn note_off(&mut self, key: u8) {
        let key = key.min(127);
        for voice in &mut self.voices {
            if voice.note == key {
                voice.release();
            }
        }
    }

    fn all_notes_off(&mut self) {
        for voice in &mut self.voices {
            voice.release();
        }
    }

    fn reset(&mut self) {
        self.voices = std::array::from_fn(|_| Voice::new());
        self.tick_pos = 0;
        self.master_now = self.control.master;
        self.master_step = 0.0;
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], context: ProcessContext) {
        if context.tempo_bpm.is_finite() && context.tempo_bpm > 0.0 {
            self.tempo_bpm = context.tempo_bpm;
        }
        if context.playing {
            self.song_beat = context.start_beat;
        }
        let beats_per_frame = f64::from(self.tempo_bpm) / (60.0 * f64::from(self.sample_rate));
        let frames = left.len().min(right.len());
        let mut done = 0;
        while done < frames {
            if self.tick_pos == 0 {
                self.tick();
            }
            let n = (TICK - self.tick_pos).min(frames - done);
            let mut mix = [[0.0f32; TICK]; 2];
            for voice in &mut self.voices {
                if !voice.env1.is_idle() {
                    voice.render(n, &self.control, &mut mix);
                }
            }
            for i in 0..n {
                self.master_now += self.master_step;
                left[done + i] += mix[0][i] * self.master_now;
                right[done + i] += mix[1][i] * self.master_now;
            }
            self.tick_pos = (self.tick_pos + n) % TICK;
            self.song_beat += n as f64 * beats_per_frame;
            done += n;
        }
    }

    fn set_tempo_bpm(&mut self, bpm: f32) {
        if bpm.is_finite() && bpm > 0.0 {
            self.tempo_bpm = bpm;
        }
    }

    fn set_controller(&mut self, index: usize, value: f32) {
        if !value.is_finite() {
            return;
        }
        let value = value.clamp(0.0, 1.0);
        match index {
            PITCH_BEND => self.control.bend = 4.0 * value - 2.0,
            MOD_WHEEL => self.control.wheel = value,
            _ => {}
        }
    }

    /// Slides add to every voice's pitch, pan and velocity from the next control tick.
    fn set_slide(&mut self, pitch: f32, pan: f32, velocity: f32) {
        let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
        for voice in &mut self.voices {
            voice.slide = [finite(pitch), finite(pan), finite(velocity)];
        }
    }
}

#[cfg(test)]
impl DewSerum {
    /// Voices that have not finished.
    fn sounding(&self) -> usize {
        self.voices.iter().filter(|v| !v.env1.is_idle()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::modulation::menu_value;
    use super::*;

    const RATE: f32 = 48_000.0;

    fn context() -> ProcessContext {
        ProcessContext {
            sample_rate: RATE,
            tempo_bpm: 120.0,
            start_beat: 0.0,
            playing: false,
        }
    }

    fn render(synth: &mut DewSerum, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0.0; frames], vec![0.0; frames]);
        synth.process(&mut l, &mut r, context());
        (l, r)
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count()
    }

    fn osc(o: usize, which: usize) -> usize {
        param::OSC_BASE[o] + which
    }

    #[test]
    fn the_synth_is_send_and_the_layout_is_consistent() {
        fn is_send<T: Send>() {}
        is_send::<DewSerum>();
        assert_eq!(PARAM_COUNT, 82);
        let p = defaults();
        assert!(p.iter().all(|v| (0.0..=1.0).contains(v)));
        assert_eq!(p[param::ENABLE], 1.0);
        assert_eq!(&p[param::LFO_CURVE..], &CurvePreset::Triangle.curve().to_params());
    }

    #[test]
    fn it_is_silent_until_a_note() {
        let mut synth = DewSerum::new(RATE);
        let (l, r) = render(&mut synth, 1024);
        assert!(l.iter().chain(&r).all(|&v| v == 0.0));
        assert_eq!(synth.sounding(), 0);
    }

    #[test]
    fn a_note_sounds_then_releases_to_silence() {
        let mut synth = DewSerum::new(RATE);
        synth.note_on(60, 1.0, 0.5);
        let (l, r) = render(&mut synth, 4800);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(rms(&l[2400..]) > 0.02, "{}", rms(&l[2400..]));
        synth.note_off(60);
        let (l, r) = render(&mut synth, 48_000);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert_eq!(synth.sounding(), 0);
        assert!(l[l.len() - 1000..].iter().chain(&r[r.len() - 1000..]).all(|&v| v == 0.0));
        // The renders add into the buffers rather than replace them.
        let (mut l, mut r) = (vec![0.25; 64], vec![-0.25; 64]);
        synth.process(&mut l, &mut r, context());
        assert!(l.iter().all(|&v| v == 0.25) && r.iter().all(|&v| v == -0.25));
    }

    #[test]
    fn the_block_size_does_not_change_the_sound() {
        let mut whole = DewSerum::new(RATE);
        let mut parts = DewSerum::new(RATE);
        for s in [&mut whole, &mut parts] {
            s.set_parameter(osc(1, param::LEVEL), 0.6);
            s.set_parameter(osc(1, param::UNISON), 1.0);
            s.note_on(57, 0.8, 0.3);
            s.note_on(64, 0.6, 0.7);
        }
        let (wl, wr) = render(&mut whole, 3000);
        let (mut pl, mut pr) = (Vec::new(), Vec::new());
        let mut done = 0;
        while done < 3000 {
            let n = 37.min(3000 - done);
            let (l, r) = render(&mut parts, n);
            pl.extend(l);
            pr.extend(r);
            done += n;
        }
        let worst = wl.iter().zip(&pl).chain(wr.iter().zip(&pr)).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst < 1e-6, "{worst}");
    }

    #[test]
    fn the_voice_limit_steals_the_oldest_note() {
        let mut synth = DewSerum::new(RATE);
        synth.set_parameter(param::VOICES, menu_value(1, MAX_VOICES));
        synth.note_on(60, 1.0, 0.5);
        synth.note_on(64, 1.0, 0.5);
        synth.note_on(67, 1.0, 0.5);
        assert_eq!(synth.sounding(), 2);
        let notes: Vec<u8> = synth.voices.iter().filter(|v| !v.env1.is_idle()).map(|v| v.note).collect();
        assert!(notes.contains(&64) && notes.contains(&67) && !notes.contains(&60), "{notes:?}");
        // With the full eight, eight notes make eight voices.
        let mut synth = DewSerum::new(RATE);
        for key in 60..68 {
            synth.note_on(key, 1.0, 0.5);
        }
        assert_eq!(synth.sounding(), MAX_VOICES);
        let (l, _) = render(&mut synth, 2048);
        assert!(l.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_released_voice_is_taken_before_a_held_one() {
        let mut synth = DewSerum::new(RATE);
        synth.set_parameter(param::VOICES, menu_value(1, MAX_VOICES));
        synth.note_on(60, 1.0, 0.5);
        synth.note_on(64, 1.0, 0.5);
        synth.note_off(64);
        synth.note_on(67, 1.0, 0.5);
        let notes: Vec<u8> = synth.voices.iter().filter(|v| !v.env1.is_idle()).map(|v| v.note).collect();
        assert!(notes.contains(&60) && notes.contains(&67), "{notes:?}");
    }

    #[test]
    fn one_unison_voice_is_centred_and_seven_spread_across_the_field() {
        let mut one = DewSerum::new(RATE);
        one.set_parameter(osc(0, param::UNISON), 0.0);
        one.note_on(60, 1.0, 0.5);
        let (l, r) = render(&mut one, 2048);
        assert!(rms(&l) > 0.01);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-7));

        let mut seven = DewSerum::new(RATE);
        seven.set_parameter(osc(0, param::UNISON), 1.0);
        seven.set_parameter(osc(0, param::WIDTH), 1.0);
        seven.note_on(60, 1.0, 0.5);
        let (l, r) = render(&mut seven, 2048);
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum::<f32>() / l.len() as f32;
        assert!(diff > 1e-3, "{diff}");
        // Width 0 puts every voice in the middle again.
        let mut mono = DewSerum::new(RATE);
        mono.set_parameter(osc(0, param::UNISON), 1.0);
        mono.set_parameter(osc(0, param::WIDTH), 0.0);
        mono.note_on(60, 1.0, 0.5);
        let (l, r) = render(&mut mono, 2048);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn unison_weights_are_power_normalized_and_blend_thins_the_outer_voices() {
        for n in 1..=MAX_UNISON {
            for blend in [0.0, 0.5, 1.0] {
                let (l, r) = unison_weights(n, blend, 0.0);
                assert_eq!(l, r);
                let power: f32 = l[..n].iter().map(|w| w * w).sum();
                assert!((power - 1.0).abs() < 1e-4, "n {n} blend {blend}: {power}");
                assert!(l[n..].iter().all(|&w| w == 0.0));
            }
        }
        // Seven voices, Blend 0: the middle one alone. Blend 1: all alike.
        let (l, _) = unison_weights(7, 0.0, 0.0);
        assert!(l[3] > 0.6 && l[0] < 0.01, "{l:?}");
        let (l, _) = unison_weights(7, 1.0, 0.0);
        assert!((l[0] - l[3]).abs() < 1e-6);
        // Two voices: Blend changes nothing.
        assert_eq!(unison_weights(2, 0.0, 0.0), unison_weights(2, 1.0, 0.0));
        // Full width: the left voice is hard left.
        let (l, r) = unison_weights(3, 1.0, 1.0);
        assert!(l[0] > 0.1 && r[0] == 0.0 && r[2] > 0.1 && l[2] == 0.0);
    }

    #[test]
    fn a_low_cutoff_darkens_the_sound() {
        let mut open = DewSerum::new(RATE);
        open.set_parameter(param::CUTOFF, 1.0);
        let mut closed = DewSerum::new(RATE);
        closed.set_parameter(param::CUTOFF, 0.2);
        for s in [&mut open, &mut closed] {
            s.note_on(69, 1.0, 0.5);
        }
        let (open_l, _) = render(&mut open, 9600);
        let (closed_l, _) = render(&mut closed, 9600);
        assert!(rms(&closed_l[4800..]) < 0.3 * rms(&open_l[4800..]));
    }

    #[test]
    fn the_filter_types_differ() {
        let render_with = |filter: f32| {
            let mut s = DewSerum::new(RATE);
            s.set_parameter(param::FILTER_TYPE, filter);
            s.set_parameter(param::CUTOFF, 0.6);
            s.note_on(48, 1.0, 0.5);
            render(&mut s, 9600).0
        };
        let (low, high, band) = (render_with(0.0), render_with(0.5), render_with(1.0));
        assert!(rms(&low[4800..]) > 0.0 && rms(&high[4800..]) > 0.0 && rms(&band[4800..]) > 0.0);
        // A low note under a cutoff well above it: the low pass passes it, the high pass thins it.
        assert!(rms(&high[4800..]) < rms(&low[4800..]));
        assert!(low.iter().zip(&band).any(|(a, b)| (a - b).abs() > 1e-3));
    }

    #[test]
    fn the_pitch_follows_the_key_the_param_and_the_bend() {
        let pure = |synth: &mut DewSerum| {
            synth.set_parameter(osc(0, param::UNISON), 0.0);
            synth.set_parameter(osc(0, param::WT_POS), 0.0);
            synth.set_parameter(param::CUTOFF, 1.0);
        };
        let count = |synth: &mut DewSerum| {
            let (l, _) = render(synth, 4800);
            crossings(&l[480..])
        };
        // A440 over 0.09 s: about 39.6 cycles.
        let mut a = DewSerum::new(RATE);
        pure(&mut a);
        a.note_on(69, 1.0, 0.5);
        assert!((38..=41).contains(&count(&mut a)));
        // An octave up by the Pitch param: about 79.
        let mut b = DewSerum::new(RATE);
        pure(&mut b);
        b.set_parameter(osc(0, param::PITCH), menu_value(36, 49));
        b.note_on(69, 1.0, 0.5);
        assert!((77..=82).contains(&count(&mut b)));
        // Two semitones up by the bend: 493.9 Hz, about 44.4.
        let mut c = DewSerum::new(RATE);
        pure(&mut c);
        c.set_controller(4, 1.0);
        c.note_on(69, 1.0, 0.5);
        assert!((43..=46).contains(&count(&mut c)));
    }

    #[test]
    fn the_lfo_in_the_matrix_moves_the_wavetable() {
        let make = |modulated: bool| {
            let mut s = DewSerum::new(RATE);
            s.set_parameter(osc(0, param::UNISON), 0.0);
            s.set_parameter(param::LFO_RATE, 0.9);
            if modulated {
                s.set_parameter(param::MATRIX, menu_value(1, 7));
                s.set_parameter(param::MATRIX + 1, menu_value(1, 11));
                s.set_parameter(param::MATRIX + 2, 1.0);
            }
            s.note_on(57, 1.0, 0.5);
            render(&mut s, 12_000).0
        };
        let (plain, moved) = (make(false), make(true));
        let diff: f32 = plain.iter().zip(&moved).map(|(a, b)| (a - b).abs()).sum::<f32>() / plain.len() as f32;
        assert!(diff > 1e-3, "{diff}");
    }

    #[test]
    fn env2_on_the_cutoff_opens_the_filter_as_it_decays() {
        let mut s = DewSerum::new(RATE);
        s.set_parameter(param::CUTOFF, 0.35);
        // Env 2 (attack 1 ms, decay about 0.1 s, sustain 0) → Cutoff, full depth.
        s.set_parameter(param::ENV2 + 1, 0.5);
        s.set_parameter(param::MATRIX, menu_value(3, 7));
        s.set_parameter(param::MATRIX + 1, menu_value(3, 11));
        s.set_parameter(param::MATRIX + 2, 1.0);
        let mut plain = DewSerum::new(RATE);
        plain.set_parameter(param::CUTOFF, 0.35);
        s.note_on(60, 1.0, 0.5);
        plain.note_on(60, 1.0, 0.5);
        let (swept, _) = render(&mut s, 2400);
        let (still, _) = render(&mut plain, 2400);
        // Early on the swept filter is far more open than the fixed one.
        assert!(rms(&swept[480..1200]) > 1.3 * rms(&still[480..1200]));
    }

    #[test]
    fn a_new_curve_reaches_the_table_without_a_note() {
        let mut s = DewSerum::new(RATE);
        assert!(s.lfo_table.value(0.0).abs() < 1e-6);
        for (i, v) in CurvePreset::Ramp.curve().to_params().iter().enumerate() {
            s.set_parameter(param::LFO_CURVE + i, *v);
        }
        render(&mut s, 64);
        assert!((s.lfo_table.value(0.0) - 1.0).abs() < 1e-6);
        assert!((s.lfo_table.value(0.999) + 1.0).abs() < 0.02);
    }

    #[test]
    fn the_lfo_is_smoothed_not_stepped() {
        let mut s = DewSerum::new(RATE);
        for (i, v) in CurvePreset::Square.curve().to_params().iter().enumerate() {
            s.set_parameter(param::LFO_CURVE + i, *v);
        }
        s.set_parameter(param::LFO_RATE, 0.95);
        s.note_on(60, 1.0, 0.5);
        let mut last = s.voices[0].lfo_smooth;
        let mut biggest = 0.0f32;
        for _ in 0..400 {
            render(&mut s, TICK);
            let now = s.voices[0].lfo_smooth;
            biggest = biggest.max((now - last).abs());
            last = now;
        }
        // The square swings 2.0 between its halves; one tick moves the smoothed value only a fraction of that.
        assert!(biggest > 0.05, "{biggest}");
        assert!(biggest < 0.45, "{biggest}");
    }

    #[test]
    fn junk_is_ignored_and_reset_silences_at_once() {
        let mut s = DewSerum::new(RATE);
        s.set_parameter(10_000, 0.5);
        s.set_parameter(param::CUTOFF, f32::NAN);
        s.set_parameter(param::CUTOFF, f32::INFINITY);
        s.set_controller(4, f32::NAN);
        s.set_controller(99, 0.5);
        s.note_on(200, f32::NAN, f32::NAN);
        let (l, r) = render(&mut s, 1024);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(rms(&l) > 0.0);
        s.reset();
        assert_eq!(s.sounding(), 0);
        let (l, r) = render(&mut s, 512);
        assert!(l.iter().chain(&r).all(|&v| v == 0.0));
    }

    #[test]
    fn all_notes_off_releases_every_voice() {
        let mut s = DewSerum::new(RATE);
        for key in [48, 55, 60] {
            s.note_on(key, 1.0, 0.5);
        }
        render(&mut s, 2048);
        s.all_notes_off();
        render(&mut s, 48_000);
        assert_eq!(s.sounding(), 0);
    }

    #[test]
    fn a_slide_shifts_the_pitch_of_a_sounding_note() {
        let pure = |s: &mut DewSerum| {
            s.set_parameter(osc(0, param::UNISON), 0.0);
            s.set_parameter(osc(0, param::WT_POS), 0.0);
            s.set_parameter(param::CUTOFF, 1.0);
        };
        let mut s = DewSerum::new(RATE);
        pure(&mut s);
        s.note_on(57, 1.0, 0.5);
        s.set_slide(12.0, 0.0, 0.0);
        let (l, _) = render(&mut s, 4800);
        // A220 slid an octave up is A440: about 39.6 cycles in 0.09 s.
        assert!((37..=41).contains(&crossings(&l[480..])));
    }
}
