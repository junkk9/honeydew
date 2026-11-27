//! DewSerum: two-oscillator wavetable synth with Sub, Noise, Warp, Glide, 4 ADSR, 8 LFO, 4 Macros & 16-slot Matrix.

pub mod fx;
pub mod modulation;
pub mod wavetable;

use std::sync::Arc;

use crate::ProcessContext;
use crate::biquad::{Biquad, Response};
use crate::instrument::Instrument;
use crate::voice::{LFO_SYNC_BEATS, lfo_rate_hz, tempo_step};

use modulation::{
    AdsrParams, CurvePreset, Envelope, Lfo, LfoCurve, LfoSettings, LfoTable, LfoTrig, ModDest,
    ModInputs, ModMatrix, SLOT_PARAMS, choice,
};
use fx::{
    FX_KIND, FX_MIX, FX_ON, FX_P1, FX_P2, FX_P3, FX_SLOT_PARAMS, FX_SLOTS, FX_SUB, FxKind,
    FxRack, SlotInputs,
};
use wavetable::{
    CUSTOM_MENU_INDEX, CustomTable, Reader, TABLE_MENU_COUNT, WavetableBank, bank,
    set_preview_custom,
};

pub const TICK: usize = 32;
pub const MAX_VOICES: usize = 8;
pub const MAX_UNISON: usize = 12;
pub const LFO_COUNT: usize = 8;
pub const ENV_COUNT: usize = 4;
pub const MACRO_COUNT: usize = 4;
const COMB_BUF_LEN: usize = 4096;

pub const SCOPE_SAMPLES: usize = 128;
pub const ANALYSIS_LEN: usize = 144;

const MAX_DETUNE_SEMITONES: f32 = 1.0;
const LFO_SMOOTH_SECONDS: f32 = 0.002;
const PITCH_BEND: usize = 4;
const MOD_WHEEL: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterKind {
    Lp12,
    Lp24,
    Hp12,
    Hp24,
    Bp12,
    Notch,
    Comb,
}

pub const FILTER_KIND_COUNT: usize = 7;

impl FilterKind {
    pub const NAMES: [&'static str; FILTER_KIND_COUNT] =
        ["LP 12", "LP 24", "HP 12", "HP 24", "BP 12", "Notch", "Comb"];

    pub fn from_param(v: f32) -> Self {
        match choice(v, FILTER_KIND_COUNT) {
            0 => Self::Lp12,
            1 => Self::Lp24,
            2 => Self::Hp12,
            3 => Self::Hp24,
            4 => Self::Bp12,
            5 => Self::Notch,
            _ => Self::Comb,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarpMode {
    Off,
    BendPlus,
    BendMinus,
    BendBipolar,
    Pwm,
    Mirror,
}

pub const WARP_MODE_COUNT: usize = 6;

impl WarpMode {
    pub const NAMES: [&'static str; WARP_MODE_COUNT] =
        ["Off", "Bend+", "Bend-", "Bend+/-", "PWM", "Mirror"];

    pub fn from_param(v: f32) -> Self {
        match choice(v, WARP_MODE_COUNT) {
            0 => Self::Off,
            1 => Self::BendPlus,
            2 => Self::BendMinus,
            3 => Self::BendBipolar,
            4 => Self::Pwm,
            _ => Self::Mirror,
        }
    }
}

#[inline]
pub fn apply_warp(phase: u32, mode: WarpMode, amount: f32) -> u32 {
    if matches!(mode, WarpMode::Off) || amount <= 1e-4 {
        return phase;
    }
    let p = (phase as f32) * (1.0 / 4_294_967_296.0);
    let a = amount.clamp(0.0, 0.98);
    let warped = match mode {
        WarpMode::Off => p,
        WarpMode::BendPlus => {
            let pivot = 0.5 * (1.0 - 0.85 * a);
            if p < pivot {
                0.5 * (p / pivot.max(1e-4))
            } else {
                0.5 + 0.5 * ((p - pivot) / (1.0 - pivot).max(1e-4))
            }
        }
        WarpMode::BendMinus => {
            let pivot = 0.5 * (1.0 + 0.85 * a);
            if p < pivot {
                0.5 * (p / pivot.max(1e-4))
            } else {
                0.5 + 0.5 * ((p - pivot) / (1.0 - pivot).max(1e-4))
            }
        }
        WarpMode::BendBipolar => {
            let k = 1.0 + 3.0 * a;
            let s = (p * std::f32::consts::TAU).sin() * (0.14 * a);
            (p + s * k * 0.35).rem_euclid(1.0)
        }
        WarpMode::Pwm => {
            let duty = (0.5 * (1.0 - 0.9 * a)).max(0.02);
            if p < duty {
                0.5 * (p / duty)
            } else {
                0.5 + 0.5 * ((p - duty) / (1.0 - duty))
            }
        }
        WarpMode::Mirror => {
            let fold = 1.0 + 3.0 * a;
            let x = (p * fold) % 2.0;
            if x <= 1.0 { x } else { 2.0 - x }
        }
    };
    (warped.clamp(0.0, 0.999_999) * 4_294_967_296.0) as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubShape {
    Sine,
    Triangle,
    Saw,
    Square,
}

pub const SUB_SHAPE_COUNT: usize = 4;

impl SubShape {
    pub const NAMES: [&'static str; SUB_SHAPE_COUNT] = ["Sine", "Triangle", "Saw", "Square"];

    pub fn from_param(v: f32) -> Self {
        match choice(v, SUB_SHAPE_COUNT) {
            0 => Self::Sine,
            1 => Self::Triangle,
            2 => Self::Saw,
            _ => Self::Square,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoiseKind {
    White,
    Pink,
    Metallic,
}

pub const NOISE_KIND_COUNT: usize = 3;

impl NoiseKind {
    pub const NAMES: [&'static str; NOISE_KIND_COUNT] = ["White", "Pink", "Metallic"];

    pub fn from_param(v: f32) -> Self {
        match choice(v, NOISE_KIND_COUNT) {
            0 => Self::White,
            1 => Self::Pink,
            _ => Self::Metallic,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceMode {
    Poly,
    Mono,
    Legato,
}

pub const VOICE_MODE_COUNT: usize = 3;

impl VoiceMode {
    pub const NAMES: [&'static str; VOICE_MODE_COUNT] = ["Poly", "Mono", "Legato"];

    pub fn from_param(v: f32) -> Self {
        match choice(v, VOICE_MODE_COUNT) {
            0 => Self::Poly,
            1 => Self::Mono,
            _ => Self::Legato,
        }
    }
}
pub mod param {
    use super::modulation::{CURVE_PARAMS, SLOT_PARAMS};
    use super::ENV_COUNT;

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

    pub const ENV_BASE: usize = 21;
    pub const ENV_PARAMS: usize = 4;

    // LFO 1..4 settings (37..=52)
    pub const LFO_BASE: usize = ENV_BASE + ENV_COUNT * ENV_PARAMS;
    pub const LFO_PARAMS: usize = 4;
    pub const LFO_RATE: usize = 0;
    pub const LFO_MODE: usize = 1;
    pub const LFO_BEAT: usize = 2;
    pub const LFO_TRIG: usize = 3;

    // Matrix slots 1..8 (53..=76)
    pub const MATRIX: usize = LFO_BASE + 4 * LFO_PARAMS;
    pub const MASTER: usize = MATRIX + 8 * SLOT_PARAMS; // 77
    pub const VOICES: usize = MASTER + 1; // 78

    // LFO 1..4 curves (79..=222)
    pub const LFO_CURVE: usize = VOICES + 1;
    pub const FILTER_DRIVE: usize = LFO_CURVE + 4 * CURVE_PARAMS; // 223
    pub const OSC_A_TABLE: usize = FILTER_DRIVE + 1; // 224
    pub const OSC_B_TABLE: usize = OSC_A_TABLE + 1; // 225
    pub const OSC_A_SYNC: usize = OSC_B_TABLE + 1; // 226
    pub const OSC_B_SYNC: usize = OSC_A_SYNC + 1; // 227
    pub const OSC_A_FM: usize = OSC_B_SYNC + 1; // 228

    // Stage 5.1 additions (229..=241)
    pub const OSC_A_WARP_MODE: usize = 229;
    pub const OSC_A_WARP: usize = 230;
    pub const OSC_B_WARP_MODE: usize = 231;
    pub const OSC_B_WARP: usize = 232;

    pub const SUB_SHAPE: usize = 233;
    pub const SUB_OCTAVE: usize = 234;
    pub const SUB_LEVEL: usize = 235;
    pub const SUB_DIRECT: usize = 236;

    pub const NOISE_KIND: usize = 237;
    pub const NOISE_COLOR: usize = 238;
    pub const NOISE_LEVEL: usize = 239;

    pub const VOICE_MODE: usize = 240;
    pub const GLIDE_TIME: usize = 241;

    // Stage 5.2 additions (242..=429): LFO 5..8 settings, LFO 5..8 curves, Matrix slots 9..16, Macros 1..4
    pub const LFO_EXT_BASE: usize = 242; // 242..=257 (4 * 4)
    pub const LFO_EXT_CURVE: usize = LFO_EXT_BASE + 4 * LFO_PARAMS; // 258..=401 (4 * 36)
    pub const MATRIX_EXT: usize = LFO_EXT_CURVE + 4 * CURVE_PARAMS; // 402..=425 (8 * 3)
    pub const MACRO_BASE: usize = MATRIX_EXT + 8 * SLOT_PARAMS; // 426..=429 (4)
    pub const FX_BASE: usize = MACRO_BASE + 4; // 430..=541 (16 * 7)
    pub const COUNT: usize = FX_BASE + 16 * 7; // 542

    pub const fn env_at(index: usize) -> usize {
        ENV_BASE + index * ENV_PARAMS
    }

    pub const fn lfo_at(index: usize) -> usize {
        if index < 4 {
            LFO_BASE + index * LFO_PARAMS
        } else {
            LFO_EXT_BASE + (index - 4) * LFO_PARAMS
        }
    }

    pub const fn curve_at(index: usize) -> usize {
        if index < 4 {
            LFO_CURVE + index * CURVE_PARAMS
        } else {
            LFO_EXT_CURVE + (index - 4) * CURVE_PARAMS
        }
    }

    pub const fn slot_at(index: usize) -> usize {
        if index < 8 {
            MATRIX + index * SLOT_PARAMS
        } else {
            MATRIX_EXT + (index - 8) * SLOT_PARAMS
        }
    }

    pub const fn macro_at(index: usize) -> usize {
        MACRO_BASE + index
    }
    
    pub const fn fx_at(slot: usize) -> usize {
        FX_BASE + slot * 7
    }
}

pub use param::COUNT as PARAM_COUNT;

pub fn defaults() -> [f32; PARAM_COUNT] {
    use param::*;
    let mut p = [0.5; COUNT];
    p[ENABLE] = 1.0;
    let oscs = [
        [0.85, 2.0 / 3.0, 0.5, 0.5, 4.0 / 11.0, 0.35, 0.75, 0.6],
        [0.0, 0.0, 0.5, 0.5, 0.0, 0.25, 1.0, 0.5],
    ];
    for (base, values) in OSC_BASE.into_iter().zip(oscs) {
        p[base..base + OSC_PARAMS].copy_from_slice(&values);
    }
    p[FILTER_TYPE] = 0.0;
    p[CUTOFF] = 0.85;
    p[RESONANCE] = 0.15;
    p[KEY_TRACK] = 0.0;
    p[FILTER_DRIVE] = 0.0;
    p[OSC_A_TABLE] = 0.0;
    p[OSC_B_TABLE] = 0.0;
    p[OSC_A_SYNC] = 0.0;
    p[OSC_B_SYNC] = 0.0;
    p[OSC_A_FM] = 0.0;

    p[OSC_A_WARP_MODE] = 0.0;
    p[OSC_A_WARP] = 0.0;
    p[OSC_B_WARP_MODE] = 0.0;
    p[OSC_B_WARP] = 0.0;

    p[SUB_SHAPE] = 0.0;
    p[SUB_OCTAVE] = 0.5;
    p[SUB_LEVEL] = 0.0;
    p[SUB_DIRECT] = 0.0;

    p[NOISE_KIND] = 0.0;
    p[NOISE_COLOR] = 0.5;
    p[NOISE_LEVEL] = 0.0;

    p[VOICE_MODE] = 0.0;
    p[GLIDE_TIME] = 0.0;

    p[env_at(0usize)..env_at(0usize) + 4].copy_from_slice(&[0.2, 0.5, 0.8, 0.4]);
    for e in 1..ENV_COUNT {
        p[env_at(e)..env_at(e) + 4].copy_from_slice(&[0.0, 0.5, 0.0, 0.5]);
    }
    for l in 0..LFO_COUNT {
        let at = lfo_at(l);
        p[at..at + 4].copy_from_slice(&[0.5, 0.0, 4.0 / 9.0, 0.0]);
    }
    for slot in 0..16 {
        let at = slot_at(slot);
        p[at..at + SLOT_PARAMS].copy_from_slice(&[0.0, 0.0, 0.5]);
    }
    for m in 0..MACRO_COUNT {
        p[macro_at(m)] = 0.0;
    }
        for slot in 0..16 {
        let at = fx_at(slot);
        p[at..at + 7].copy_from_slice(&[0.0, 1.0, 0.0, 0.5, 0.5, 0.5, 0.5]);
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

#[derive(Clone, Copy, Default)]
struct VoiceTelemetry {
    cutoff_norm: f32,
    resonance: f32,
    wt_pos: [f32; 2],
    pitch_mod: [f32; 2],
    sync: [f32; 2],
    fm_depth: f32,
    warp: [f32; 2],
    detune: [f32; 2],
    level: [f32; 2],
}

#[derive(Clone, Copy)]
struct OscControl {
    table: usize,
    level: f32,
    wt_pos: f32,
    semitones: f32,
    unison: usize,
    detune: f32,
    sync: f32,
    warp_mode: WarpMode,
    warp: f32,
    left: [f32; MAX_UNISON],
    right: [f32; MAX_UNISON],
}

#[derive(Clone, Copy)]
struct LfoBaseCtrl {
    rate: f32,
    sync: bool,
    beat: f32,
    trig: LfoTrig,
}

#[derive(Clone, Copy)]
struct Control {
    osc: [OscControl; 2],
    fm_from_b: f32,
    sub_shape: SubShape,
    sub_octave: f32,
    sub_level: f32,
    sub_direct: bool,
    noise_kind: NoiseKind,
    noise_color: f32,
    noise_level: f32,
    voice_mode: VoiceMode,
    glide_time: f32,
    macros: [f32; MACRO_COUNT],
    lfo_ctrl: [LfoBaseCtrl; LFO_COUNT],
    env1: AdsrParams,
    env_mod: [AdsrParams; 3],
    matrix: ModMatrix,
    filter_kind: FilterKind,
    cutoff_oct: f32,
    resonance: f32,
    key_track: f32,
    drive: f32,
    max_cutoff: f32,
    lfo_coef: f32,
    master: f32,
    voice_limit: usize,
    bend: f32,
    wheel: f32,
    fx_slots: [SlotInputs; FX_SLOTS],
}
impl Control {
    fn build(p: &[f32; PARAM_COUNT], sample_rate: f32, bend: f32, wheel: f32) -> Self {
        let table_params = [param::OSC_A_TABLE, param::OSC_B_TABLE];
        let sync_params = [param::OSC_A_SYNC, param::OSC_B_SYNC];
        let warp_mode_params = [param::OSC_A_WARP_MODE, param::OSC_B_WARP_MODE];
        let warp_params = [param::OSC_A_WARP, param::OSC_B_WARP];
        let osc = std::array::from_fn(|o| {
            let at = param::OSC_BASE[o];
            let unison = 1 + choice(p[at + param::UNISON], MAX_UNISON);
            let (left, right) = unison_weights(unison, p[at + param::BLEND], p[at + param::WIDTH]);
            OscControl {
                table: choice(p[table_params[o]], TABLE_MENU_COUNT),
                level: p[at + param::LEVEL],
                wt_pos: p[at + param::WT_POS],
                semitones: semitones(p[at + param::PITCH]) + (2.0 * p[at + param::FINE] - 1.0),
                unison,
                detune: p[at + param::DETUNE],
                sync: p[sync_params[o]].clamp(0.0, 1.0),
                warp_mode: WarpMode::from_param(p[warp_mode_params[o]]),
                warp: p[warp_params[o]].clamp(0.0, 1.0),
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
        let sub_oct = match choice(p[param::SUB_OCTAVE], 3) {
            0 => -24.0,
            1 => -12.0,
            _ => 0.0,
        };
        let mut slot_buf = [0.0f32; 16 * SLOT_PARAMS];
        for s in 0..16 {
            let at = param::slot_at(s);
            slot_buf[s * SLOT_PARAMS..(s + 1) * SLOT_PARAMS]
                .copy_from_slice(&p[at..at + SLOT_PARAMS]);
        }
        let lfo_ctrl = std::array::from_fn(|l| {
            let at = param::lfo_at(l);
            LfoBaseCtrl {
                rate: p[at + param::LFO_RATE].clamp(0.0, 1.0),
                sync: p[at + param::LFO_MODE] >= 0.5,
                beat: p[at + param::LFO_BEAT].clamp(0.0, 1.0),
                trig: LfoTrig::from_param(p[at + param::LFO_TRIG]),
            }
        });
                let macros = std::array::from_fn(|m| p[param::macro_at(m)].clamp(0.0, 1.0));
        let fx_slots = std::array::from_fn(|s| {
            let at = param::fx_at(s);
            SlotInputs {
                kind: FxKind::from_param(p[at + FX_KIND]),
                active: p[at + FX_ON] >= 0.5,
                sub: p[at + FX_SUB].clamp(0.0, 1.0),
                p1: p[at + FX_P1].clamp(0.0, 1.0),
                p2: p[at + FX_P2].clamp(0.0, 1.0),
                p3: p[at + FX_P3].clamp(0.0, 1.0),
                mix: p[at + FX_MIX].clamp(0.0, 1.0),
            }
        });
            Self {
            osc,
            fm_from_b: p[param::OSC_A_FM].clamp(0.0, 1.0),
            sub_shape: SubShape::from_param(p[param::SUB_SHAPE]),
            sub_octave: sub_oct,
            sub_level: p[param::SUB_LEVEL].clamp(0.0, 1.0),
            sub_direct: p[param::SUB_DIRECT] >= 0.5,
            noise_kind: NoiseKind::from_param(p[param::NOISE_KIND]),
            noise_color: p[param::NOISE_COLOR].clamp(0.0, 1.0),
            noise_level: p[param::NOISE_LEVEL].clamp(0.0, 1.0),
            voice_mode: VoiceMode::from_param(p[param::VOICE_MODE]),
            glide_time: p[param::GLIDE_TIME].clamp(0.0, 1.0),
            macros,
            fx_slots,
            lfo_ctrl,
            env1: adsr(0usize, sample_rate),
            env_mod: [
                adsr(1usize, tick_rate),
                adsr(2usize, tick_rate),
                adsr(3usize, tick_rate),
            ],
            matrix: ModMatrix::from_params(&slot_buf),
            filter_kind: FilterKind::from_param(p[param::FILTER_TYPE]),
            cutoff_oct: 20f32.log2() + 10.0 * p[param::CUTOFF],
            resonance: p[param::RESONANCE],
            key_track: p[param::KEY_TRACK],
            drive: 1.0 + 7.0 * p[param::FILTER_DRIVE].clamp(0.0, 1.0),
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
const SYNC_DEST: [ModDest; 2] = [ModDest::OscASync, ModDest::OscBSync];
const WARP_DEST: [ModDest; 2] = [ModDest::OscAWarp, ModDest::OscBWarp];
const LFO_RATE_DEST: [ModDest; LFO_COUNT] = [
    ModDest::Lfo1Rate,
    ModDest::Lfo2Rate,
    ModDest::Lfo3Rate,
    ModDest::Lfo4Rate,
    ModDest::Lfo5Rate,
    ModDest::Lfo6Rate,
    ModDest::Lfo7Rate,
    ModDest::Lfo8Rate,
];

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

    #[inline]
    fn next_f32(&mut self) -> f32 {
        (self.next_u32() as i32 as f32) * (1.0 / 2_147_483_648.0)
    }
}

struct Voice {
    note: u8,
    current_pitch: f32,
    target_pitch: f32,
    velocity: f32,
    pan: f32,
    slide: [f32; 3],
    age: u64,
    env1: Envelope,
    env_mod: [Envelope; 3],
    lfo: [Lfo; LFO_COUNT],
    lfo_smooth: [f32; LFO_COUNT],
    filter1: [Biquad; 2],
    filter2: [Biquad; 2],
    comb_buf: [[f32; COMB_BUF_LEN]; 2],
    comb_pos: usize,
    comb_delay: f32,
    comb_fb: f32,
    phase: [[u32; MAX_UNISON]; 2],
    inc: [[u32; MAX_UNISON]; 2],
    sync_mult: [u64; 2],
    warp_amt: [f32; 2],
    fm_amount: f32,
    sub_phase: u32,
    sub_inc: u32,
    sub_gain: f32,
    noise_gain: f32,
    noise_lp: f32,
    noise_hp: f32,
    noise_alpha: f32,
    pink_b: [f32; 3],
    mip_level: [[usize; MAX_UNISON]; 2],
    morph_pos: [f32; 2],
    gain: [f32; 2],
    gain_step: [f32; 2],
    vel_gain: f32,
    pan_gain: [f32; 2],
    telemetry: VoiceTelemetry,
    fresh: bool,
}

impl Voice {
    fn new() -> Self {
        Self {
            note: 0,
            current_pitch: 60.0,
            target_pitch: 60.0,
            velocity: 0.0,
            pan: 0.5,
            slide: [0.0; 3],
            age: 0,
            env1: Envelope::default(),
            env_mod: [Envelope::default(); 3],
            lfo: [Lfo::default(); LFO_COUNT],
            lfo_smooth: [0.0; LFO_COUNT],
            filter1: [Biquad::default(); 2],
            filter2: [Biquad::default(); 2],
            comb_buf: [[0.0; COMB_BUF_LEN]; 2],
            comb_pos: 0,
            comb_delay: 64.0,
            comb_fb: 0.0,
            phase: [[0; MAX_UNISON]; 2],
            inc: [[0; MAX_UNISON]; 2],
            sync_mult: [65536; 2],
            warp_amt: [0.0; 2],
            fm_amount: 0.0,
            sub_phase: 0,
            sub_inc: 0,
            sub_gain: 0.0,
            noise_gain: 0.0,
            noise_lp: 0.0,
            noise_hp: 0.0,
            noise_alpha: 0.5,
            pink_b: [0.0; 3],
            mip_level: [[0; MAX_UNISON]; 2],
            morph_pos: [0.0; 2],
            gain: [0.0; 2],
            gain_step: [0.0; 2],
            vel_gain: 0.0,
            pan_gain: [1.0; 2],
            telemetry: VoiceTelemetry::default(),
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
        prev_pitch: Option<f32>,
        legato_retrig_skip: bool,
        lfo_settings: &[LfoSettings; LFO_COUNT],
        rng: &mut Rng,
    ) {
        let was_idle = self.env1.is_idle();
        if was_idle {
            self.env1.reset();
            for f in &mut self.filter1 {
                f.reset();
            }
            for f in &mut self.filter2 {
                f.reset();
            }
            self.comb_buf = [[0.0; COMB_BUF_LEN]; 2];
            self.comb_pos = 0;
            self.sub_phase = 0;
            self.noise_lp = 0.0;
            self.noise_hp = 0.0;
            self.pink_b = [0.0; 3];
            for phases in &mut self.phase {
                for (k, phase) in phases.iter_mut().enumerate() {
                    *phase = if k == 0 { 0 } else { rng.next_u32() };
                }
            }
        }
        self.note = note;
        self.target_pitch = f32::from(note);
        self.current_pitch = prev_pitch.unwrap_or(self.target_pitch);
        self.velocity = velocity;
        self.pan = pan;
        self.slide = [0.0; 3];
        self.age = age;
        if was_idle || !legato_retrig_skip {
            self.env1.gate_on();
            for env in &mut self.env_mod {
                env.reset();
                env.gate_on();
            }
            for l in 0..LFO_COUNT {
                self.lfo[l].start(lfo_settings[l].start_phase);
            }
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
      #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        c: &Control,
        tables: &[LfoTable; LFO_COUNT],
        lfos: &[LfoSettings; LFO_COUNT],
        tempo_bpm: f32,
        sample_rate: f32,
        advance: bool,
    ) {
        for l in 0..LFO_COUNT {
            let raw = self.lfo[l].value(&tables[l]);
            if self.fresh {
                self.lfo_smooth[l] = raw;
            } else {
                self.lfo_smooth[l] += (raw - self.lfo_smooth[l]) * c.lfo_coef;
            }
        }

        let pre_pitch = self.current_pitch + self.slide[0usize] + c.bend;
        let inputs = ModInputs {
            lfo: self.lfo_smooth,
            env: [
                self.env1.level(),
                self.env_mod[0usize].level(),
                self.env_mod[1usize].level(),
                self.env_mod[2usize].level(),
            ],
            macros: c.macros,
            velocity: self.velocity,
            key: ModInputs::key_value(pre_pitch),
            wheel: c.wheel,
        };
        let m = c.matrix.evaluate(&inputs);

        if advance {
            for (env, params) in self.env_mod.iter_mut().zip(&c.env_mod) {
                env.advance(params);
            }
                let tick_sec = (TICK as f64) / f64::from(sample_rate.max(1.0));
            for l in 0..LFO_COUNT {
                let one_shot = lfos[l].trig == LfoTrig::Env;
                let rate_mod = m.get(LFO_RATE_DEST[l]);
                let step = if rate_mod.abs() > 1e-5 {
                    if c.lfo_ctrl[l].sync {
                        let bpm = if tempo_bpm.is_finite() && tempo_bpm > 0.0 {
                            tempo_bpm
                        } else {
                            120.0
                        };
                        let beat_v = (c.lfo_ctrl[l].beat + rate_mod).clamp(0.0, 1.0);
                        let beats = f64::from(tempo_step(&LFO_SYNC_BEATS, beat_v));
                        (f64::from(bpm) / (60.0 * beats) * tick_sec).min(0.5)
                    } else {
                        let rate_v = (c.lfo_ctrl[l].rate + rate_mod).clamp(0.0, 1.0);
                        (f64::from(lfo_rate_hz(rate_v)) * tick_sec).min(0.5)
                    }
                } else {
                    lfos[l].step
                };
                self.lfo[l].advance(step, one_shot);
            }
        }

        let glide_norm = (c.glide_time + m.get(ModDest::GlideTime)).clamp(0.0, 1.0);
        if glide_norm <= 1e-4 {
            self.current_pitch = self.target_pitch;
        } else if advance {
            let glide_sec = 0.002 + glide_norm * glide_norm * 0.998;
            let tick_sec = TICK as f32 / sample_rate;
            let alpha = 1.0 - (-tick_sec / glide_sec).exp();
            self.current_pitch += (self.target_pitch - self.current_pitch) * alpha;
        }

        let pitch = self.current_pitch + self.slide[0usize] + c.bend;

        for o in 0..2 {
            let osc = &c.osc[o];
            let sync_val = (osc.sync + m.get(SYNC_DEST[o])).clamp(0.0, 1.0);
            let ratio = 1.0 + 15.0 * sync_val * sync_val;
            self.sync_mult[o] = (ratio * 65536.0) as u64;
            let warp_val = (osc.warp + m.get(WARP_DEST[o])).clamp(0.0, 1.0);
            self.warp_amt[o] = warp_val;

            let morph_pos = (osc.wt_pos + m.get(WT_DEST[o])).clamp(0.0, 1.0);
            let pitch_mod = m.get(PITCH_DEST[o]);
            let note = (pitch + osc.semitones + pitch_mod).clamp(-48.0, 160.0);
            let detune = (osc.detune + m.get(DETUNE_DEST[o])).clamp(0.0, 1.0);
            let spread = detune * detune * MAX_DETUNE_SEMITONES;
            let level = (osc.level + m.get(LEVEL_DEST[o])).clamp(0.0, 1.0);

            self.morph_pos[o] = morph_pos;
            self.telemetry.wt_pos[o] = morph_pos;
            self.telemetry.pitch_mod[o] = pitch_mod;
            self.telemetry.sync[o] = sync_val;
            self.telemetry.warp[o] = warp_val;
            self.telemetry.detune[o] = detune;
            self.telemetry.level[o] = level;

            for k in 0..osc.unison {
                let semitone = note + spread * unison_position(osc.unison, k);
                let freq = (440.0 * ((semitone - 69.0) / 12.0).exp2()).min(0.49 * sample_rate);
                self.inc[o][k] =
                    (f64::from(freq) / f64::from(sample_rate) * 4_294_967_296.0) as u32;
                let effective_freq = (freq * ratio).min(0.49 * sample_rate);
                self.mip_level[o][k] = WavetableBank::level_for(effective_freq, sample_rate);
            }
            let target = level * level;
            if self.fresh {
                self.gain[o] = target;
                self.gain_step[o] = 0.0;
            } else {
                self.gain_step[o] = (target - self.gain[o]) / TICK as f32;
            }
        }

        let sub_lvl = (c.sub_level + m.get(ModDest::SubLevel)).clamp(0.0, 1.0);
        self.sub_gain = sub_lvl * sub_lvl;
        let sub_note = (pitch + c.sub_octave).clamp(-48.0, 160.0);
        let sub_freq = (440.0 * ((sub_note - 69.0) / 12.0).exp2()).min(0.49 * sample_rate);
        self.sub_inc = (f64::from(sub_freq) / f64::from(sample_rate) * 4_294_967_296.0) as u32;

        let noise_lvl = (c.noise_level + m.get(ModDest::NoiseLevel)).clamp(0.0, 1.0);
        self.noise_gain = noise_lvl * noise_lvl * 0.65;
        let color = (c.noise_color + m.get(ModDest::NoiseColor)).clamp(0.0, 1.0);
        let cutoff_n = 80.0 * 2.0f32.powf(7.8 * color);
        self.noise_alpha = (1.0 - (-std::f32::consts::TAU * cutoff_n / sample_rate).exp()).clamp(0.005, 0.995);

        let fm_val = (c.fm_from_b + m.get(ModDest::OscAFm)).clamp(0.0, 1.0);
        self.fm_amount = fm_val * fm_val * 0.5;
        self.telemetry.fm_depth = fm_val;

        let cutoff_oct = c.cutoff_oct
            + m.get(ModDest::Cutoff)
            + c.key_track * (pitch - 60.0) / 12.0;
        let cutoff = cutoff_oct.exp2().clamp(20.0, c.max_cutoff);
        let res = (c.resonance + m.get(ModDest::Resonance)).clamp(0.0, 1.0);
        self.telemetry.cutoff_norm = ((cutoff.log2() - 20f32.log2()) / 10.0).clamp(0.0, 1.0);
        self.telemetry.resonance = res;

        let q = 0.5 * 20f32.powf(res);
        let q24 = 0.5 * 8f32.powf(res);

        match c.filter_kind {
            FilterKind::Lp12 => {
                for f in &mut self.filter1 {
                    f.set_filter_response(Response::LowPass, cutoff, q, sample_rate);
                }
            }
            FilterKind::Lp24 => {
                for ch in 0..2 {
                    self.filter1[ch].set_filter_response(Response::LowPass, cutoff, q24, sample_rate);
                    self.filter2[ch].set_filter_response(Response::LowPass, cutoff, q24, sample_rate);
                }
            }
            FilterKind::Hp12 => {
                for f in &mut self.filter1 {
                    f.set_filter_response(Response::HighPass, cutoff, q, sample_rate);
                }
            }
            FilterKind::Hp24 => {
                for ch in 0..2 {
                    self.filter1[ch].set_filter_response(Response::HighPass, cutoff, q24, sample_rate);
                    self.filter2[ch].set_filter_response(Response::HighPass, cutoff, q24, sample_rate);
                }
            }
            FilterKind::Bp12 => {
                for f in &mut self.filter1 {
                    f.set_filter_response(Response::BandPass, cutoff, q, sample_rate);
                }
            }
            FilterKind::Notch => {
                let bw = (1.2 - 0.9 * res).max(0.15);
                for f in &mut self.filter1 {
                    f.set_parametric_response(Response::Peak, cutoff, bw, -36.0, sample_rate);
                }
            }
            FilterKind::Comb => {
                self.comb_delay = (sample_rate / cutoff).clamp(2.0, (COMB_BUF_LEN - 2) as f32);
                self.comb_fb = 0.25 + 0.72 * res;
            }
        }

        self.vel_gain = (self.velocity + self.slide[2usize]).clamp(0.0, 1.0);
        let pan = (self.pan + self.slide[1usize]).clamp(0.0, 1.0);
        self.pan_gain = [(2.0 - 2.0 * pan).min(1.0), (2.0 * pan).min(1.0)];
        self.fresh = false;
            }
      #[inline]
    fn filter_sample(&mut self, ch: usize, x: f32, kind: FilterKind) -> f32 {
        match kind {
            FilterKind::Lp12 | FilterKind::Hp12 | FilterKind::Bp12 | FilterKind::Notch => {
                self.filter1[ch].process(x)
            }
            FilterKind::Lp24 | FilterKind::Hp24 => {
                let y = self.filter1[ch].process(x);
                self.filter2[ch].process(y)
            }
            FilterKind::Comb => {
                let d_int = self.comb_delay as usize;
                let frac = self.comb_delay - d_int as f32;
                let i0 = (self.comb_pos + COMB_BUF_LEN - d_int) % COMB_BUF_LEN;
                let i1 = (i0 + COMB_BUF_LEN - 1) % COMB_BUF_LEN;
                let delayed = self.comb_buf[ch][i0] * (1.0 - frac) + self.comb_buf[ch][i1] * frac;
                let out = (x + delayed * self.comb_fb).tanh();
                self.comb_buf[ch][self.comb_pos] = out;
                out
            }
        }
    }

    #[inline]
    fn sub_sample(&mut self, shape: SubShape) -> f32 {
        self.sub_phase = self.sub_phase.wrapping_add(self.sub_inc);
        let p = (self.sub_phase as f32) * (1.0 / 4_294_967_296.0);
        match shape {
            SubShape::Sine => (p * std::f32::consts::TAU).sin(),
            SubShape::Triangle => 1.0 - 4.0 * (p - 0.5).abs(),
            SubShape::Saw => 2.0 * p - 1.0,
            SubShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        }
    }

    #[inline]
    fn noise_sample(&mut self, kind: NoiseKind, rng: &mut Rng) -> f32 {
        let white = rng.next_f32();
        let raw = match kind {
            NoiseKind::White => white,
            NoiseKind::Pink => {
                self.pink_b[0usize] = 0.997_65 * self.pink_b[0usize] + white * 0.099_046;
                self.pink_b[1usize] = 0.963_00 * self.pink_b[1usize] + white * 0.296_516;
                self.pink_b[2usize] = 0.570_00 * self.pink_b[2usize] + white * 1.052_691;
                (self.pink_b[0usize] + self.pink_b[1usize] + self.pink_b[2usize] + white * 0.1848)
                    * 0.22
            }
            NoiseKind::Metallic => {
                let s = rng.next_u32();
                let bit = ((s >> 7) ^ (s >> 13) ^ (s >> 19)) & 1;
                if bit == 1 { 0.85 } else { -0.85 }
            }
        };
        self.noise_lp += (raw - self.noise_lp) * self.noise_alpha;
        self.noise_hp += (self.noise_lp - self.noise_hp) * (self.noise_alpha * 0.25);
        self.noise_lp - self.noise_hp * 0.45
    }

    fn render(
        &mut self,
        n: usize,
        c: &Control,
        custom: &[Option<Arc<CustomTable>>; 2],
        rng: &mut Rng,
        mix: &mut [[f32; TICK]; 2],
    ) {
        let drive = c.drive;
        let use_drive = drive > 1.001;
        let fm_amt = self.fm_amount;
        let use_fm = fm_amt > 1e-6;
        let use_sub = self.sub_gain > 1e-6;
        let use_noise = self.noise_gain > 1e-6;

        let wt = bank();
        let readers: [[Reader<'_>; MAX_UNISON]; 2] = std::array::from_fn(|o| {
            let osc = &c.osc[o];
            let pos = self.morph_pos[o];
            std::array::from_fn(|k| {
                let lvl = self.mip_level[o][k];
                if osc.table == CUSTOM_MENU_INDEX {
                    wt.reader_custom(custom[o].as_deref(), lvl, pos)
                } else {
                    wt.reader(osc.table, lvl, pos)
                }
            })
        });

        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            let gain_b = self.gain[1usize];
            let need_b = gain_b != 0.0 || self.gain_step[1usize] != 0.0 || use_fm;
            let (mut sum_bl, mut sum_br, mut mod_b) = (0.0f32, 0.0f32, 0.0f32);
            if need_b {
                let osc_b = &c.osc[1usize];
                let mult_b = self.sync_mult[1usize];
                let w_mode_b = osc_b.warp_mode;
                let w_amt_b = self.warp_amt[1usize];
                for k in 0..osc_b.unison {
                    let phase = self.phase[1usize][k].wrapping_add(self.inc[1usize][k]);
                    self.phase[1usize][k] = phase;
                    let synced = ((u64::from(phase) * mult_b) >> 16) as u32;
                    let warped = apply_warp(synced, w_mode_b, w_amt_b);
                    let s = readers[1usize][k].read(warped);
                    sum_bl += s * osc_b.left[k];
                    sum_br += s * osc_b.right[k];
                }
                mod_b = 0.5 * (sum_bl + sum_br);
            }
            self.gain[1usize] = gain_b + self.gain_step[1usize];

            let gain_a = self.gain[0usize];
            let (mut sum_al, mut sum_ar) = (0.0f32, 0.0f32);
            if gain_a != 0.0 || self.gain_step[0usize] != 0.0 {
                let osc_a = &c.osc[0usize];
                let mult_a = self.sync_mult[0usize];
                let w_mode_a = osc_a.warp_mode;
                let w_amt_a = self.warp_amt[0usize];
                let fm_offset = if use_fm {
                    ((mod_b * fm_amt) * 4_294_967_296.0) as i64 as u32
                } else {
                    0
                };
                for k in 0..osc_a.unison {
                    let phase = self.phase[0usize][k].wrapping_add(self.inc[0usize][k]);
                    self.phase[0usize][k] = phase;
                    let synced = ((u64::from(phase) * mult_a) >> 16) as u32;
                    let warped = apply_warp(synced, w_mode_a, w_amt_a);
                    let s = readers[0usize][k].read(warped.wrapping_add(fm_offset));
                    sum_al += s * osc_a.left[k];
                    sum_ar += s * osc_a.right[k];
                }
            }
            self.gain[0usize] = gain_a + self.gain_step[0usize];

            let sub = if use_sub {
                self.sub_sample(c.sub_shape) * self.sub_gain
            } else {
                0.0
            };
            let noise = if use_noise {
                self.noise_sample(c.noise_kind, rng) * self.noise_gain
            } else {
                0.0
            };

            let mut l = sum_al * gain_a + sum_bl * gain_b + noise;
            let mut r = sum_ar * gain_a + sum_br * gain_b + noise;
            if !c.sub_direct {
                l += sub;
                r += sub;
            }

            if use_drive {
                l = (l * drive).tanh();
                r = (r * drive).tanh();
            }
            let mut fl = self.filter_sample(0usize, l, c.filter_kind);
            let mut fr = self.filter_sample(1usize, r, c.filter_kind);
            if c.sub_direct {
                fl += sub;
                fr += sub;
            }
            if matches!(c.filter_kind, FilterKind::Comb) {
                self.comb_pos = (self.comb_pos + 1) % COMB_BUF_LEN;
            }
            let amp = self.env1.advance(&c.env1) * self.vel_gain;
            mix[0usize][i] += fl * amp * self.pan_gain[0usize];
            mix[1usize][i] += fr * amp * self.pan_gain[1usize];
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
    custom_tables: [Option<Arc<CustomTable>>; 2],
    dirty: bool,
    lfo_tables: [LfoTable; LFO_COUNT],
    curve_dirty: [bool; LFO_COUNT],
    lfo_settings: [LfoSettings; LFO_COUNT],
    free_phases: [f64; LFO_COUNT],
    voices: [Voice; MAX_VOICES],
    last_voice: usize,
    rng: Rng,
    note_count: u64,
    tempo_bpm: f32,
    song_beat: f64,
    tick_pos: usize,
    master_now: f32,
    master_step: f32,
    scope_ring: [f32; SCOPE_SAMPLES],
    scope_pos: usize,
    analysis_buf: [0.0; ANALYSIS_LEN],
    fx_rack: FxRack::new(sample_rate),
    fx_mod_inputs: control.fx_slots,
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
            custom_tables: [None, None],
            dirty: false,
            lfo_tables,
            curve_dirty: [false; LFO_COUNT],
            lfo_settings,
            free_phases: [0.0; LFO_COUNT],
            voices: std::array::from_fn(|_| Voice::new()),
            last_voice: 0,
            rng: Rng(0x9E37_79B9),
            note_count: 0,
            tempo_bpm: 120.0,
            song_beat: 0.0,
            tick_pos: 0,
            master_step: 0.0,
            scope_ring: [0.0; SCOPE_SAMPLES],
            scope_pos: 0,
            analysis_buf: [0.0; ANALYSIS_LEN],
            fx_rack: FxRack::new(sample_rate),
            fx_mod_inputs: control.fx_slots,
        }
        }
    }

    pub fn set_custom_wavetable(&mut self, osc: usize, table: Option<Arc<CustomTable>>) {
        let idx = osc.min(1);
        set_preview_custom(idx, table.clone());
        self.custom_tables[idx] = table;
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
        let (control, tables, lfos, bpm, rate) = (
            &self.control,
            &self.lfo_tables,
            &self.lfo_settings,
            self.tempo_bpm,
            self.sample_rate,
        );
        for voice in &mut self.voices {
            if !voice.env1.is_idle() {
                voice.update(control, tables, lfos, bpm, rate, true);
            }
        }
          self.master_step = (self.control.master - self.master_now) / TICK as f32;
        let mut fx_now = self.control.fx_slots;
        let active_v = if !self.voices[self.last_voice].env1.is_idle() {
            Some(self.last_voice)
        } else {
            self.voices.iter().position(|v| !v.env1.is_idle())
        };
        let inputs = if let Some(idx) = active_v {
            let v = &self.voices[idx];
            ModInputs {
                lfo: v.lfo_smooth,
                env: [
                    v.env1.level(),
                    v.env_mod[0usize].level(),
                    v.env_mod[1usize].level(),
                    v.env_mod[2usize].level(),
                ],
                macros: self.control.macros,
                velocity: v.velocity,
                key: ModInputs::key_value(v.current_pitch),
                wheel: self.control.wheel,
            }
        } else {
            ModInputs {
                macros: self.control.macros,
                wheel: self.control.wheel,
                ..ModInputs::default()
            }
        };
        let fx_m = self.control.matrix.evaluate(&inputs);
        for (s, slot) in fx_now.iter_mut().enumerate() {
            if !slot.active || matches!(slot.kind, FxKind::Empty) {
                continue;
            }
            slot.p1 = (slot.p1 + fx_m.get(ModDest::fx_param(s, 0))).clamp(0.0, 1.0);
            slot.p2 = (slot.p2 + fx_m.get(ModDest::fx_param(s, 1))).clamp(0.0, 1.0);
            slot.p3 = (slot.p3 + fx_m.get(ModDest::fx_param(s, 2))).clamp(0.0, 1.0);
            slot.mix = (slot.mix + fx_m.get(ModDest::fx_param(s, 3))).clamp(0.0, 1.0);
        }
        self.fx_mod_inputs = fx_now;
    }

    fn pick_voice(&self, limit: usize) -> usize {
        if !matches!(self.control.voice_mode, VoiceMode::Poly) {
            return 0;
        }
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
        let mono_mode = !matches!(self.control.voice_mode, VoiceMode::Poly);
        let index = self.pick_voice(self.control.voice_limit);
        let prev_pitch = if mono_mode && !self.voices[index].env1.is_idle() {
            Some(self.voices[index].current_pitch)
        } else if self.control.glide_time > 1e-4 && !self.voices[self.last_voice].env1.is_idle() {
            Some(self.voices[self.last_voice].current_pitch)
        } else {
            None
        };
        let legato_skip = matches!(self.control.voice_mode, VoiceMode::Legato)
            && self.voices[index].env1.is_held();
        self.last_voice = index;
        self.note_count += 1;
        let voice = &mut self.voices[index];
        voice.start(
            key,
            velocity,
            pan,
            self.note_count,
            prev_pitch,
            legato_skip,
            &lfos,
            &mut self.rng,
        );
        voice.update(
            &self.control,
            &self.lfo_tables,
            &lfos,
            self.tempo_bpm,
            self.sample_rate,
            false,
        );
            }
        fn update_analysis(&mut self) {
        for i in 0..SCOPE_SAMPLES {
            self.analysis_buf[i] = self.scope_ring[(self.scope_pos + i) % SCOPE_SAMPLES];
        }
        let active_idx = if !self.voices[self.last_voice].env1.is_idle() {
            Some(self.last_voice)
        } else {
            self.voices.iter().position(|v| !v.env1.is_idle())
        };
        if let Some(idx) = active_idx {
            let t = self.voices[idx].telemetry;
            let base = SCOPE_SAMPLES;
            let vals = [
                t.cutoff_norm,
                t.resonance,
                t.wt_pos[0usize],
                t.wt_pos[1usize],
                t.pitch_mod[0usize],
                t.pitch_mod[1usize],
                t.sync[0usize],
                t.sync[1usize],
                t.fm_depth,
                t.warp[0usize],
                t.warp[1usize],
                t.level[0usize],
                t.level[1usize],
                1.0,
            ];
            self.analysis_buf[base..base + vals.len()].copy_from_slice(&vals);
        } else {
            self.analysis_buf[SCOPE_SAMPLES..ANALYSIS_LEN].fill(0.0);
            
        }
    }
}

impl Instrument for DewSerum {
    fn set_parameter(&mut self, index: usize, value: f32) {
        if index >= PARAM_COUNT || !value.is_finite() {
            return;
        }
        self.params[index] = value.clamp(0.0, 1.0);
        if (index >= param::LFO_CURVE && index < param::FILTER_DRIVE)
            || (index >= param::LFO_EXT_CURVE && index < param::MATRIX_EXT)
        {
            let lfo_idx = if index < param::FILTER_DRIVE {
                (index - param::LFO_CURVE) / modulation::CURVE_PARAMS
            } else {
                4 + (index - param::LFO_EXT_CURVE) / modulation::CURVE_PARAMS
            };
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
        self.scope_ring.fill(0.0);
        self.analysis_buf.fill(0.0);
        self.fx_rack.reset();
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
                    voice.render(n, &self.control, &self.custom_tables, &mut self.rng, &mut mix);
                }
            }
                        for i in 0..n {
                let (fx_l, fx_r) = self.fx_rack.process_frame(
                    mix[0usize][i],
                    mix[1usize][i],
                    &self.fx_mod_inputs,
                    self.tempo_bpm,
                );
                self.master_now += self.master_step;
                let out_l = fx_l * self.master_now;
                let out_r = fx_r * self.master_now;
                left[done + i] += out_l;
                right[done + i] += out_r;
                if (done + i) & 1 == 0 {
                    self.scope_ring[self.scope_pos] = 0.5 * (out_l + out_r);
                    self.scope_pos = (self.scope_pos + 1) % SCOPE_SAMPLES;
                }
            }
            self.tick_pos = (self.tick_pos + n) % TICK;
            self.song_beat += n as f64 * beats_per_frame;
            done += n;
        }
        self.update_analysis();
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

    fn analysis(&self) -> &[f32] {
        &self.analysis_buf
    }
                                                    }
                
