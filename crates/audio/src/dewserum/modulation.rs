//! DewSerum's modulation: the ADSR envelopes (Env 1 on the amp, Env 2..4 free), the four curve LFOs
//! with Trig/Env/Free phase control, and the eight-slot matrix that routes a source to a destination.

use crate::voice::{LFO_SYNC_BEATS, lfo_rate_hz, tempo_step};

const EPSILON: f32 = 1e-4;
const SIXTY_DB_LN: f32 = 6.907_755;

fn unit(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

pub fn choice(v: f32, count: usize) -> usize {
    let last = count.saturating_sub(1);
    ((unit(v) * last as f32 + 0.5) as usize).min(last)
}

pub fn menu_value(index: usize, count: usize) -> f32 {
    index.min(count.saturating_sub(1)) as f32 / count.saturating_sub(1).max(1) as f32
}

// ---------------------------------------------------------------------------------------------------------------------
// Envelopes
// ---------------------------------------------------------------------------------------------------------------------

pub fn envelope_seconds(v: f32) -> f32 {
    0.001 * 10_000f32.powf(unit(v))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AdsrParams {
    attack_step: f32,
    decay_coef: f32,
    pub sustain: f32,
    release_coef: f32,
}

impl AdsrParams {
    pub fn new(attack: f32, decay: f32, sustain: f32, release: f32, rate: f32) -> Self {
        Self::from_seconds(
            envelope_seconds(attack),
            envelope_seconds(decay),
            unit(sustain),
            envelope_seconds(release),
            rate,
        )
    }

    pub fn from_seconds(attack: f32, decay: f32, sustain: f32, release: f32, rate: f32) -> Self {
        let steps = |seconds: f32| (seconds.max(0.0) * rate).max(1.0);
        Self {
            attack_step: 1.0 / steps(attack),
            decay_coef: (-SIXTY_DB_LN / steps(decay)).exp(),
            sustain: unit(sustain),
            release_coef: (-SIXTY_DB_LN / steps(release)).exp(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    #[default]
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Envelope {
    level: f32,
    stage: Stage,
}

impl Envelope {
    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    pub fn is_held(&self) -> bool {
        matches!(
            self.stage,
            Stage::Attack | Stage::Decay | Stage::Sustain
        )
    }

    #[inline]
    pub fn advance(&mut self, params: &AdsrParams) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += params.attack_step;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let distance = (self.level - params.sustain) * params.decay_coef;
                if distance.abs() < EPSILON {
                    self.level = params.sustain;
                    self.stage = Stage::Sustain;
                } else {
                    self.level = params.sustain + distance;
                }
            }
            Stage::Sustain => self.level = params.sustain,
            Stage::Release => {
                self.level *= params.release_coef;
                if self.level < EPSILON {
                    self.level = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.level
    }
}
// ---------------------------------------------------------------------------------------------------------------------
// LFO: a point-and-curve shape the player draws
// ---------------------------------------------------------------------------------------------------------------------

pub const MAX_POINTS: usize = 12;
pub const MIN_POINTS: usize = 2;
pub const SEGMENTS: usize = MAX_POINTS - 1;
pub const CURVE_PARAMS: usize = 1 + 2 * MAX_POINTS + SEGMENTS;
pub const TABLE_LEN: usize = 256;

pub const fn x_param(i: usize) -> usize {
    1 + 2 * i
}

pub const fn y_param(i: usize) -> usize {
    2 + 2 * i
}

pub const fn bend_param(i: usize) -> usize {
    1 + 2 * MAX_POINTS + i
}

fn bend_t(t: f32, k: f32, rising: bool) -> f32 {
    if k == 0.0 {
        return t;
    }
    let e = 8f32.powf(k);
    if rising { t.powf(1.0 / e) } else { t.powf(e) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoCurve {
    count: usize,
    x: [f32; MAX_POINTS],
    y: [f32; MAX_POINTS],
    bend: [f32; SEGMENTS],
}

impl Default for LfoCurve {
    fn default() -> Self {
        CurvePreset::Triangle.curve()
    }
}

impl LfoCurve {
    pub fn from_points(points: &[(f32, f32)], bends: &[f32]) -> Self {
        let mut curve = Self {
            count: points.len().clamp(MIN_POINTS, MAX_POINTS),
            x: [0.0; MAX_POINTS],
            y: [0.0; MAX_POINTS],
            bend: [0.0; SEGMENTS],
        };
        for (i, &(x, y)) in points.iter().take(curve.count).enumerate() {
            curve.x[i] = x;
            curve.y[i] = y;
        }
        for (i, &k) in bends.iter().take(curve.count - 1).enumerate() {
            curve.bend[i] = k;
        }
        curve.sanitize();
        curve
    }

    #[allow(clippy::needless_range_loop)]
    pub fn from_params(params: &[f32]) -> Self {
        if params.len() < CURVE_PARAMS {
            return Self::default();
        }
        let mut curve = Self {
            count: MIN_POINTS + choice(params[0], MAX_POINTS - MIN_POINTS + 1),
            x: [0.0; MAX_POINTS],
            y: [0.0; MAX_POINTS],
            bend: [0.0; SEGMENTS],
        };
        for i in 0..curve.count {
            curve.x[i] = params[x_param(i)];
            curve.y[i] = params[y_param(i)];
        }
        for i in 0..curve.count - 1 {
            curve.bend[i] = 2.0 * unit(params[bend_param(i)]) - 1.0;
        }
        curve.sanitize();
        curve
    }

    #[allow(clippy::needless_range_loop)]
    pub fn to_params(&self) -> [f32; CURVE_PARAMS] {
        let mut params = [0.5; CURVE_PARAMS];
        params[0] = menu_value(self.count - MIN_POINTS, MAX_POINTS - MIN_POINTS + 1);
        for i in 0..MAX_POINTS {
            let active = i < self.count;
            params[x_param(i)] = if active { self.x[i] } else { 1.0 };
            params[y_param(i)] = if active { self.y[i] } else { 0.5 };
        }
        for i in 0..self.count - 1 {
            params[bend_param(i)] = 0.5 * (self.bend[i] + 1.0);
        }
        params
    }

    #[allow(clippy::needless_range_loop)]
    fn sanitize(&mut self) {
        let n = self.count.clamp(MIN_POINTS, MAX_POINTS);
        self.count = n;
        for i in 0..MAX_POINTS {
            if i < n {
                self.x[i] = unit(self.x[i]);
                self.y[i] = unit(self.y[i]);
            } else {
                self.x[i] = 0.0;
                self.y[i] = 0.0;
            }
        }
        self.x[0] = 0.0;
        self.x[n - 1] = 1.0;
        for i in 1..n {
            self.x[i] = self.x[i].max(self.x[i - 1]);
        }
        for i in 0..SEGMENTS {
            let k = self.bend[i];
            self.bend[i] = if i < n - 1 && k.is_finite() {
                k.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn point(&self, i: usize) -> (f32, f32) {
        let i = i.min(self.count - 1);
        (self.x[i], self.y[i])
    }

    pub fn bend(&self, i: usize) -> f32 {
        if i + 1 < self.count { self.bend[i] } else { 0.0 }
    }

    pub fn value(&self, x: f32) -> f32 {
        let x = unit(x);
        let n = self.count;
        let mut i = 0;
        while i + 1 < n && self.x[i + 1] <= x {
            i += 1;
        }
        if i + 1 >= n {
            return self.y[n - 1];
        }
        let t = ((x - self.x[i]) / (self.x[i + 1] - self.x[i])).clamp(0.0, 1.0);
        let (y0, y1) = (self.y[i], self.y[i + 1]);
        y0 + (y1 - y0) * bend_t(t, self.bend[i], y1 >= y0)
    }

    pub fn insert(&mut self, x: f32, y: f32) -> Option<usize> {
        if self.count == MAX_POINTS {
            return None;
        }
        let x = unit(x);
        let mut at = 1;
        while at < self.count - 1 && self.x[at] <= x {
            at += 1;
        }
        for j in (at..self.count).rev() {
            self.x[j + 1] = self.x[j];
            self.y[j + 1] = self.y[j];
        }
        for j in (at..self.count - 1).rev() {
            self.bend[j + 1] = self.bend[j];
        }
        self.bend[at] = self.bend[at - 1];
        self.count += 1;
        self.x[at] = x.clamp(self.x[at - 1], self.x[at + 1]);
        self.y[at] = unit(y);
        Some(at)
    }

    pub fn remove(&mut self, i: usize) -> bool {
        if self.count <= MIN_POINTS || i == 0 || i >= self.count - 1 {
            return false;
        }
        for j in i..self.count - 1 {
            self.x[j] = self.x[j + 1];
            self.y[j] = self.y[j + 1];
        }
        for j in i..self.count - 2 {
            self.bend[j] = self.bend[j + 1];
        }
        self.count -= 1;
        self.x[self.count] = 0.0;
        self.y[self.count] = 0.0;
        self.bend[self.count - 1] = 0.0;
        true
    }

    pub fn move_point(&mut self, i: usize, x: f32, y: f32) {
        if i >= self.count {
            return;
        }
        self.y[i] = unit(y);
        if i > 0 && i < self.count - 1 {
            self.x[i] = unit(x).clamp(self.x[i - 1], self.x[i + 1]);
        }
    }

    pub fn set_bend(&mut self, i: usize, k: f32) {
        if i + 1 < self.count && k.is_finite() {
            self.bend[i] = k.clamp(-1.0, 1.0);
        }
    }
                    }
   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurvePreset {
    Triangle,
    Saw,
    Ramp,
    Pluck,
    Sine,
    Square,
}

pub const PRESET_COUNT: usize = 6;

impl CurvePreset {
    pub const ALL: [CurvePreset; PRESET_COUNT] = [
        Self::Triangle,
        Self::Saw,
        Self::Ramp,
        Self::Pluck,
        Self::Sine,
        Self::Square,
    ];
    pub const NAMES: [&'static str; PRESET_COUNT] =
        ["Triangle", "Saw", "Ramp", "Pluck", "Sine", "Square"];

    pub fn curve(self) -> LfoCurve {
        match self {
            Self::Triangle => {
                LfoCurve::from_points(&[(0.0, 0.5), (0.25, 1.0), (0.75, 0.0), (1.0, 0.5)], &[])
            }
            Self::Saw => LfoCurve::from_points(&[(0.0, 0.0), (1.0, 1.0)], &[]),
            Self::Ramp => LfoCurve::from_points(&[(0.0, 1.0), (1.0, 0.0)], &[]),
            Self::Pluck => LfoCurve::from_points(&[(0.0, 1.0), (1.0, 0.0)], &[-0.5]),
            Self::Sine => LfoCurve::from_points(
                &[(0.0, 0.5), (0.25, 1.0), (0.5, 0.5), (0.75, 0.0), (1.0, 0.5)],
                &[0.25, 0.25, -0.25, -0.25],
            ),
            Self::Square => LfoCurve::from_points(
                &[(0.0, 1.0), (0.5, 1.0), (0.5, 0.0), (1.0, 0.0)],
                &[],
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoTable {
    values: [f32; TABLE_LEN + 1],
}

impl Default for LfoTable {
    fn default() -> Self {
        Self::from_curve(&LfoCurve::default())
    }
}

impl LfoTable {
    pub fn from_curve(curve: &LfoCurve) -> Self {
        let mut table = Self {
            values: [0.0; TABLE_LEN + 1],
        };
        table.rebuild(curve);
        table
    }

    pub fn rebuild(&mut self, curve: &LfoCurve) {
        for (k, cell) in self.values.iter_mut().enumerate() {
            *cell = 2.0 * curve.value(k as f32 / TABLE_LEN as f32) - 1.0;
        }
    }

    pub fn value(&self, phase: f64) -> f32 {
        if !phase.is_finite() {
            return self.values[0usize];
        }
        let position = phase.rem_euclid(1.0) * TABLE_LEN as f64;
        let i = (position as usize).min(TABLE_LEN - 1);
        let t = (position - i as f64) as f32;
        let a = self.values[i];
        a + (self.values[i + 1] - a) * t
    }
}

pub fn lfo_preview(params: &[f32], out: &mut [f32]) {
    let table = LfoTable::from_curve(&LfoCurve::from_params(params));
    let n = out.len();
    for (k, y) in out.iter_mut().enumerate() {
        *y = table.value(k as f64 / n as f64);
    }
                                                }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoTrig {
    Trig,
    Env,
    Free,
}

impl LfoTrig {
    pub const NAMES: [&'static str; 3] = ["Trig", "Env", "Free"];

    pub fn from_param(v: f32) -> Self {
        match choice(v, 3) {
            0 => Self::Trig,
            1 => Self::Env,
            _ => Self::Free,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoSettings {
    pub sync: bool,
    pub trig: LfoTrig,
    pub step: f64,
    pub start_phase: f64,
}

impl LfoSettings {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rate: f32,
        mode: f32,
        beat: f32,
        trig: f32,
        tempo_bpm: f32,
        song_beat: f64,
        free_phase: f64,
        sample_rate: f32,
        tick_frames: usize,
    ) -> Self {
        let tick_seconds = tick_frames as f64 / f64::from(sample_rate.max(1.0));
        let sync = unit(mode) >= 0.5;
        let trig_mode = LfoTrig::from_param(trig);
        let (step, grid_phase) = if sync {
            let bpm = if tempo_bpm.is_finite() && tempo_bpm > 0.0 {
                tempo_bpm
            } else {
                120.0
            };
            let beats = f64::from(tempo_step(&LFO_SYNC_BEATS, unit(beat)));
            let cycles_per_second = f64::from(bpm) / (60.0 * beats);
            let phase = if song_beat.is_finite() {
                (song_beat / beats).rem_euclid(1.0)
            } else {
                0.0
            };
            (cycles_per_second * tick_seconds, phase)
        } else {
            (f64::from(lfo_rate_hz(unit(rate))) * tick_seconds, free_phase.rem_euclid(1.0))
        };
        let start_phase = match trig_mode {
            LfoTrig::Trig | LfoTrig::Env => 0.0,
            LfoTrig::Free => grid_phase,
        };
        Self {
            sync,
            trig: trig_mode,
            step: step.min(0.5),
            start_phase,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lfo {
    phase: f64,
    finished: bool,
}

impl Lfo {
    pub fn start(&mut self, phase: f64) {
        self.phase = phase.rem_euclid(1.0);
        self.finished = false;
    }

    pub fn phase(&self) -> f64 {
        self.phase
    }

    pub fn value(&self, table: &LfoTable) -> f32 {
        if self.finished {
            table.value(0.999_999)
        } else {
            table.value(self.phase)
        }
    }

    pub fn advance(&mut self, step: f64, one_shot: bool) {
        if one_shot {
            if !self.finished {
                let next = self.phase + step;
                if next >= 1.0 {
                    self.phase = 1.0;
                    self.finished = true;
                } else {
                    self.phase = next;
                }
            }
        } else {
            self.phase = (self.phase + step).rem_euclid(1.0);
        }
    }
                                                                   }
// ---------------------------------------------------------------------------------------------------------------------
// Matrix
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModSource {
    None,
    Lfo1,
    Lfo2,
    Lfo3,
    Lfo4,
    Env1,
    Env2,
    Env3,
    Env4,
    Velocity,
    Key,
    ModWheel,
}

pub const SOURCE_COUNT: usize = 12;

impl ModSource {
    pub const ALL: [ModSource; SOURCE_COUNT] = [
        Self::None,
        Self::Lfo1,
        Self::Lfo2,
        Self::Lfo3,
        Self::Lfo4,
        Self::Env1,
        Self::Env2,
        Self::Env3,
        Self::Env4,
        Self::Velocity,
        Self::Key,
        Self::ModWheel,
    ];
    pub const NAMES: [&'static str; SOURCE_COUNT] = [
        "None",
        "LFO 1",
        "LFO 2",
        "LFO 3",
        "LFO 4",
        "Env 1",
        "Env 2",
        "Env 3",
        "Env 4",
        "Velocity",
        "Key",
        "Mod Wheel",
    ];

    pub fn from_param(v: f32) -> Self {
        Self::ALL[choice(v, SOURCE_COUNT)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModDest {
    None = 0,
    OscAWtPos,
    OscBWtPos,
    Cutoff,
    Resonance,
    OscAPitch,
    OscBPitch,
    OscALevel,
    OscBLevel,
    OscADetune,
    OscBDetune,
    OscASync,
    OscBSync,
    OscAFm,
}

pub const DEST_COUNT: usize = 14;

impl ModDest {
    pub const ALL: [ModDest; DEST_COUNT] = [
        Self::None,
        Self::OscAWtPos,
        Self::OscBWtPos,
        Self::Cutoff,
        Self::Resonance,
        Self::OscAPitch,
        Self::OscBPitch,
        Self::OscALevel,
        Self::OscBLevel,
        Self::OscADetune,
        Self::OscBDetune,
        Self::OscASync,
        Self::OscBSync,
        Self::OscAFm,
    ];
    pub const NAMES: [&'static str; DEST_COUNT] = [
        "None",
        "A WT Pos",
        "B WT Pos",
        "Cutoff",
        "Resonance",
        "A Pitch",
        "B Pitch",
        "A Level",
        "B Level",
        "A Detune",
        "B Detune",
        "A Sync",
        "B Sync",
        "A FM Depth",
    ];

    pub fn from_param(v: f32) -> Self {
        Self::ALL[choice(v, DEST_COUNT)]
    }

    pub fn scale(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Cutoff => 5.0,
            Self::OscAPitch | Self::OscBPitch => 12.0,
            _ => 1.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModInputs {
    pub lfo: [f32; 4],
    pub env: [f32; 4],
    pub velocity: f32,
    pub key: f32,
    pub wheel: f32,
}

impl ModInputs {
    pub fn key_value(note: f32) -> f32 {
        ((note - 60.0) / 60.0).clamp(-1.0, 1.0)
    }

    pub fn source(&self, source: ModSource) -> f32 {
        match source {
            ModSource::None => 0.0,
            ModSource::Lfo1 => self.lfo[0usize],
            ModSource::Lfo2 => self.lfo[1usize],
            ModSource::Lfo3 => self.lfo[2usize],
            ModSource::Lfo4 => self.lfo[3usize],
            ModSource::Env1 => self.env[0usize],
            ModSource::Env2 => self.env[1usize],
            ModSource::Env3 => self.env[2usize],
            ModSource::Env4 => self.env[3usize],
            ModSource::Velocity => self.velocity,
            ModSource::Key => self.key,
            ModSource::ModWheel => self.wheel,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub source: ModSource,
    pub dest: ModDest,
    pub depth: f32,
}

impl Slot {
    pub const OFF: Slot = Slot {
        source: ModSource::None,
        dest: ModDest::None,
        depth: 0.0,
    };

    pub fn from_params(source: f32, dest: f32, depth: f32) -> Self {
        Self {
            source: ModSource::from_param(source),
            dest: ModDest::from_param(dest),
            depth: if depth.is_finite() {
                2.0 * unit(depth) - 1.0
            } else {
                0.0
            },
        }
    }
}

pub const SLOTS: usize = 8;
pub const SLOT_PARAMS: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModValues([f32; DEST_COUNT]);

impl ModValues {
    pub fn get(&self, dest: ModDest) -> f32 {
        self.0[dest as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModMatrix {
    pub slots: [Slot; SLOTS],
}

impl Default for ModMatrix {
    fn default() -> Self {
        Self {
            slots: [Slot::OFF; SLOTS],
        }
    }
}

impl ModMatrix {
    pub fn from_params(params: &[f32]) -> Self {
        let mut matrix = Self::default();
        for (slot, p) in matrix.slots.iter_mut().zip(params.chunks_exact(SLOT_PARAMS)) {
            *slot = Slot::from_params(p[0usize], p[1usize], p[2usize]);
        }
        matrix
    }

    pub fn evaluate(&self, inputs: &ModInputs) -> ModValues {
        let mut values = ModValues::default();
        for slot in &self.slots {
            if slot.source == ModSource::None || slot.dest == ModDest::None || slot.depth == 0.0 {
                continue;
            }
            let amount = inputs.source(slot.source) * slot.depth * slot.dest.scale();
            if amount.is_finite() {
                values.0[slot.dest as usize] += amount;
            }
        }
        values
    }
}
