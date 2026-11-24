//! DewSerum: a two-oscillator wavetable synth in the style of Xfer Serum, built into Honeydew in pure Rust.

pub mod modulation;
pub mod wavetable;

use crate::ProcessContext;
use crate::biquad::{Biquad, Response};
use crate::instrument::Instrument;

use modulation::{
    AdsrParams, CurvePreset, Envelope, Lfo, LfoCurve, LfoSettings, LfoTable, LfoTrig, ModDest,
    ModInputs, ModMatrix, SLOT_PARAMS, SLOTS, choice,
};
use wavetable::{Morph, Reader, WavetableBank, bank};

pub const TICK: usize = 32;
pub const MAX_VOICES: usize = 8;
pub const MAX_UNISON: usize = 7;
pub const LFO_COUNT: usize = 4;
pub const ENV_COUNT: usize = 4;

const MAX_DETUNE_SEMITONES: f32 = 1.0;
const LFO_SMOOTH_SECONDS: f32 = 0.002;
const PITCH_BEND: usize = 4;
const MOD_WHEEL: usize = 8;

pub mod param {
    use super::modulation::{CURVE_PARAMS, SLOT_PARAMS, SLOTS};
    use super::{ENV_COUNT, LFO_COUNT};

    pub const ENABLE: usize = 0;
    pub const OSC_BASE: [usize; 2] = [1usize, 9usize];
    pub const OSC_PARAMS: usize = 8;
    pub const LEVEL: usize = 0;
    pub const WT_POS: usize = 1;
    pub const PITCH: usize = 2;
    pub const FINE: usize = 3;
    pub const UNISON: usize = 4;
    pub const DETUNE: usize = 5;
    pub const BLEND: usize = 6;
    pub const WIDTH: usize = 7;

    pub const FILTER_TYPE: usize = 17;
    pub const CUTOFF: usize = 18;
    pub const RESONANCE: usize = 19;
    pub const KEY_TRACK: usize = 20;

    /// 4 envelopes x 4 params (Attack, Decay, Sustain, Release): 21..37
    pub const ENV_BASE: usize = 21;
    pub const ENV_PARAMS: usize = 4;

    /// 4 LFOs x 4 params (Rate, Mode, Beat, Trig): 37..53
    pub const LFO_BASE: usize = ENV_BASE + ENV_COUNT * ENV_PARAMS;
    pub const LFO_PARAMS: usize = 4;
    pub const LFO_RATE: usize = 0;
    pub const LFO_MODE: usize = 1;
    pub const LFO_BEAT: usize = 2;
    pub const LFO_TRIG: usize = 3;

    /// 8 slots x 3 params (Source, Dest, Depth): 53..77
    pub const MATRIX: usize = LFO_BASE + LFO_COUNT * LFO_PARAMS;
    pub const MASTER: usize = MATRIX + SLOTS * SLOT_PARAMS;
    pub const VOICES: usize = MASTER + 1;

    /// 4 LFO curves x 36 params: 79..223
    pub const LFO_CURVE: usize = VOICES + 1;
    pub const COUNT: usize = LFO_CURVE + LFO_COUNT * CURVE_PARAMS;

    pub const fn env_at(index: usize) -> usize {
        ENV_BASE + index * ENV_PARAMS
    }

    pub const fn lfo_at(index: usize) -> usize {
        LFO_BASE + index * LFO_PARAMS
    }

    pub const fn curve_at(index: usize) -> usize {
        LFO_CURVE + index * CURVE_PARAMS
    }
}

pub use param::COUNT as PARAM_COUNT;

pub fn defaults() -> [f32; PARAM_COUNT] {
    use param::*;
    let mut p = [0.5; COUNT];
    p[ENABLE] = 1.0;
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

    p[env_at(0)..env_at(0) + 4].copy_from_slice(&[0.2, 0.5, 0.8, 0.4]);
    for e in 1..ENV_COUNT {
        p[env_at(e)..env_at(e) + 4].copy_from_slice(&[0.0, 0.5, 0.0, 0.5]);
    }
    for l in 0..LFO_COUNT {
        let at = lfo_at(l);
        // Rate = 0.5, Mode = Hertz (0.0), Beat = 1/1 (4/9), Trig = Trig (0.0 - no random phase)
        p[at..at + 4].copy_from_slice(&[0.5, 0.0, 4.0 / 9.0, 0.0]);
    }
    for slot in 0..SLOTS {
        let at = MATRIX + slot * SLOT_PARAMS;
        p[at..at + SLOT_PARAMS].copy_from_slice(&[0.0, 0.0, 0.5]);
    }
    p[MASTER] = 0.7;
    p[VOICES] = 1.0;
    let default_curve = CurvePreset::Triangle.curve().to_params();
    for l in 0..LFO_COUNT {
        let at = curve_at(l);
        p[at..at + modulation::CURVE_PARAMS].copy_from_slice(&default_curve);
    }
    p
}
// ---------------------------------------------------------------------------------------------------------------------
// Param laws & Control
// ---------------------------------------------------------------------------------------------------------------------

fn semitones(v: f32) -> f32 {
    (v.clamp(0.0, 1.0) * 48.0).round() - 24.0
}

fn unison_position(n: usize, k: usize) -> f32 {
    if n <= 1 {
        0.0
    } else {
        2.0 * k as f32 / (n - 1) as f32 - 1.0
    }
}

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

#[derive(Clone, Copy)]
struct OscControl {
    level: f32,
    wt_pos: f32,
    semitones: f32,
    unison: usize,
    detune: f32,
    left: [f32; MAX_UNISON],
    right: [f32; MAX_UNISON],
}

#[derive(Clone, Copy)]
struct Control {
    osc: [OscControl; 2],
    env1: AdsrParams,
    env_mod: [AdsrParams; 3],
    matrix: ModMatrix,
    response: Response,
    cutoff_oct: f32,
    resonance: f32,
    key_track: f32,
    max_cutoff: f32,
    lfo_coef: f32,
    master: f32,
    voice_limit: usize,
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
        let adsr = |idx: usize, rate: f32| {
            let at = param::env_at(idx);
            AdsrParams::new(p[at], p[at + 1], p[at + 2], p[at + 3], rate)
        };
        let tick_rate = sample_rate / TICK as f32;
        let master = p[param::MASTER];
        Self {
            osc,
            env1: adsr(0usize, sample_rate),
            env_mod: [
                adsr(1usize, tick_rate),
                adsr(2usize, tick_rate),
                adsr(3usize, tick_rate),
            ],
            matrix: ModMatrix::from_params(
                &p[param::MATRIX..param::MATRIX + SLOTS * SLOT_PARAMS],
            ),
            response: match choice(p[param::FILTER_TYPE], 3) {
                0 => Response::LowPass,
                1 => Response::HighPass,
                _ => Response::BandPass,
            },
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
    pan: f32,
    slide: [f32; 3],
    age: u64,
    env1: Envelope,
    env_mod: [Envelope; 3],
    lfo: [Lfo; LFO_COUNT],
    lfo_smooth: [f32; LFO_COUNT],
    filter: [Biquad; 2],
    phase: [[u32; MAX_UNISON]; 2],
    inc: [[u32; MAX_UNISON]; 2],
    readers: [[Reader<'static>; MAX_UNISON]; 2],
    gain: [f32; 2],
    gain_step: [f32; 2],
    vel_gain: f32,
    pan_gain: [f32; 2],
    fresh: bool,
}

impl Voice {
    fn new() -> Self {
        let idle = bank().reader(0usize, Morph::new(0.0));
        Self {
            note: 0,
            velocity: 0.0,
            pan: 0.5,
            slide: [0.0; 3],
            age: 0,
            env1: Envelope::default(),
            env_mod: [Envelope::default(); 3],
            lfo: [Lfo::default(); LFO_COUNT],
            lfo_smooth: [0.0; LFO_COUNT],
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

    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        note: u8,
        velocity: f32,
        pan: f32,
        age: u64,
        lfo_settings: &[LfoSettings; LFO_COUNT],
        rng: &mut Rng,
    ) {
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
        for env in &mut self.env_mod {
            env.reset();
            env.gate_on();
        }
        for l in 0..LFO_COUNT {
            self.lfo[l].start(lfo_settings[l].start_phase);
        }
        self.fresh = true;
    }

    fn release(&mut self) {
        if self.env1.is_held() {
            self.env1.gate_off();
            for env in &mut self.env_mod {
                env.gate_off();
            }
        }
    }

    fn update(
        &mut self,
        c: &Control,
        tables: &[LfoTable; LFO_COUNT],
        lfos: &[LfoSettings; LFO_COUNT],
        sample_rate: f32,
        advance: bool,
    ) {
        if advance {
            for (env, params) in self.env_mod.iter_mut().zip(&c.env_mod) {
                env.advance(params);
            }
            for l in 0..LFO_COUNT {
                let one_shot = lfos[l].trig == LfoTrig::Env;
                self.lfo[l].advance(lfos[l].step, one_shot);
            }
        }
        for l in 0..LFO_COUNT {
            let raw = self.lfo[l].value(&tables[l]);
            if self.fresh {
                self.lfo_smooth[l] = raw;
            } else {
                self.lfo_smooth[l] += (raw - self.lfo_smooth[l]) * c.lfo_coef;
            }
        }
        let pitch = f32::from(self.note) + self.slide[0usize] + c.bend;
        let inputs = ModInputs {
            lfo: self.lfo_smooth,
            env: [
                self.env1.level(),
                self.env_mod[0usize].level(),
                self.env_mod[1usize].level(),
                self.env_mod[2usize].level(),
            ],
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

        self.vel_gain = (self.velocity + self.slide[2usize]).clamp(0.0, 1.0);
        let pan = (self.pan + self.slide[1usize]).clamp(0.0, 1.0);
        self.pan_gain = [(2.0 - 2.0 * pan).min(1.0), (2.0 * pan).min(1.0)];
        self.fresh = false;
    }

    fn render(&mut self, n: usize, c: &Control, mix: &mut [[f32; TICK]; 2]) {
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for o in 0..2 {
                let gain = self.gain[o];
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
            mix[0usize][i] += self.filter[0usize].process(l) * amp * self.pan_gain[0usize];
            mix[1usize][i] += self.filter[1usize].process(r) * amp * self.pan_gain[1usize];
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
    dirty: bool,
    lfo_tables: [LfoTable; LFO_COUNT],
    curve_dirty: [bool; LFO_COUNT],
    lfo_settings: [LfoSettings; LFO_COUNT],
    free_phases: [f64; LFO_COUNT],
    voices: [Voice; MAX_VOICES],
    rng: Rng,
    note_count: u64,
    tempo_bpm: f32,
    song_beat: f64,
    tick_pos: usize,
    master_now: f32,
    master_step: f32,
}

impl DewSerum {
    pub fn new(sample_rate: f32) -> Self {
        let sample_rate = if sample_rate.is_finite() && sample_rate >= 8_000.0 {
            sample_rate
        } else {
            48_000.0
        };
        let _ = bank();
        let params = defaults();
        let control = Control::build(&params, sample_rate, 0.0, 0.0);
        let lfo_tables = std::array::from_fn(|l| {
            let at = param::curve_at(l);
            LfoTable::from_curve(&LfoCurve::from_params(
                &params[at..at + modulation::CURVE_PARAMS],
            ))
        });
        let lfo_settings = std::array::from_fn(|l| {
            let at = param::lfo_at(l);
            LfoSettings::new(
                params[at + param::LFO_RATE],
                params[at + param::LFO_MODE],
                params[at + param::LFO_BEAT],
                params[at + param::LFO_TRIG],
                120.0,
                0.0,
                0.0,
                sample_rate,
                TICK,
            )
        });
        Self {
            sample_rate,
            params,
            master_now: control.master,
            control,
            dirty: false,
            lfo_tables,
            curve_dirty: [false; LFO_COUNT],
            lfo_settings,
            free_phases: [0.0; LFO_COUNT],
            voices: std::array::from_fn(|_| Voice::new()),
            rng: Rng(0x9E37_79B9),
            note_count: 0,
            tempo_bpm: 120.0,
            song_beat: 0.0,
            tick_pos: 0,
            master_step: 0.0,
        }
    }

    fn refresh(&mut self) {
        for l in 0..LFO_COUNT {
            if self.curve_dirty[l] {
                let at = param::curve_at(l);
                let curve = LfoCurve::from_params(
                    &self.params[at..at + modulation::CURVE_PARAMS],
                );
                self.lfo_tables[l].rebuild(&curve);
                self.curve_dirty[l] = false;
            }
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

    fn lfo_settings_now(&self) -> [LfoSettings; LFO_COUNT] {
        std::array::from_fn(|l| {
            let at = param::lfo_at(l);
            LfoSettings::new(
                self.params[at + param::LFO_RATE],
                self.params[at + param::LFO_MODE],
                self.params[at + param::LFO_BEAT],
                self.params[at + param::LFO_TRIG],
                self.tempo_bpm,
                self.song_beat,
                self.free_phases[l],
                self.sample_rate,
                TICK,
            )
        })
    }

    fn tick(&mut self) {
        self.refresh();
        self.lfo_settings = self.lfo_settings_now();
        for l in 0..LFO_COUNT {
            self.free_phases[l] = (self.free_phases[l] + self.lfo_settings[l].step).rem_euclid(1.0);
        }
        let (control, tables, lfos, rate) = (
            &self.control,
            &self.lfo_tables,
            &self.lfo_settings,
            self.sample_rate,
        );
        for voice in &mut self.voices {
            if !voice.env1.is_idle() {
                voice.update(control, tables, lfos, rate, true);
            }
        }
        self.master_step = (self.control.master - self.master_now) / TICK as f32;
    }

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
        let lfos = self.lfo_settings_now();
        self.lfo_settings = lfos;
        let index = self.pick_voice(self.control.voice_limit);
        self.note_count += 1;
        let voice = &mut self.voices[index];
        voice.start(key, velocity, pan, self.note_count, &lfos, &mut self.rng);
        voice.update(&self.control, &self.lfo_tables, &lfos, self.sample_rate, false);
    }
}

impl Instrument for DewSerum {
    fn set_parameter(&mut self, index: usize, value: f32) {
        if index >= PARAM_COUNT || !value.is_finite() {
            return;
        }
        self.params[index] = value.clamp(0.0, 1.0);
        if index >= param::LFO_CURVE {
            let lfo_idx = (index - param::LFO_CURVE) / modulation::CURVE_PARAMS;
            if lfo_idx < LFO_COUNT {
                self.curve_dirty[lfo_idx] = true;
            }
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
                left[done + i] += mix[0usize][i] * self.master_now;
                right[done + i] += mix[1usize][i] * self.master_now;
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

    fn set_slide(&mut self, pitch: f32, pan: f32, velocity: f32) {
        let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
        for voice in &mut self.voices {
            voice.slide = [finite(pitch), finite(pan), finite(velocity)];
        }
    }
                }
            
