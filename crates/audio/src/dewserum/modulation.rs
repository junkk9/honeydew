//! DewSerum's modulation: the ADSR envelopes (Env 1 on the amp, Env 2 free), the LFO, whose shape is a curve the player
//! draws point by point, and the four-slot matrix that routes a source to a destination.
//!
//! Everything here is plain data and arithmetic: no allocation, no locks, so the voices can call it from the audio
//! thread. The module knows nothing of the param layout; `dewserum/mod.rs` hands it the normalized (0..=1) values.
//!
//! Rates: an [`Envelope`] steps once per call to [`Envelope::advance`], so Env 1 runs per sample and Env 2 per control
//! tick; [`AdsrParams`] takes the rate it will be stepped at. The LFO and the matrix run once per control tick.

use crate::voice::{LFO_SYNC_BEATS, lfo_rate_hz, tempo_step};

/// Below this a level is silence (−80 dB): an envelope that decays or releases to it snaps to its target, so a tail
/// ends instead of creeping toward it, and no subnormal float ever circulates.
const EPSILON: f32 = 1e-4;
/// ln(1000): an exponential segment of `t` seconds falls 60 dB (to a thousandth of its distance) in `t`.
const SIXTY_DB_LN: f32 = 6.907_755;

/// A normalized value, with NaN read as 0.
fn unit(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

/// The index a normalized menu value `v` picks among `count` entries: entry `i` sits at `i / (count − 1)`, and the
/// value rounds to the nearest entry. [`menu_value`] is its inverse, for defaults and presets.
pub fn choice(v: f32, count: usize) -> usize {
    let last = count.saturating_sub(1);
    ((unit(v) * last as f32 + 0.5) as usize).min(last)
}

/// The normalized value of menu entry `index` among `count`.
pub fn menu_value(index: usize, count: usize) -> f32 {
    index.min(count.saturating_sub(1)) as f32 / count.saturating_sub(1).max(1) as f32
}

// ---------------------------------------------------------------------------------------------------------------------
// Envelopes
// ---------------------------------------------------------------------------------------------------------------------

/// The time of an Attack, Decay or Release param `v`: 1 ms at 0 up to 10 s at 1, exponential (a decade per quarter of
/// the knob's travel). The UI shows this number.
pub fn envelope_seconds(v: f32) -> f32 {
    0.001 * 10_000f32.powf(unit(v))
}

/// An ADSR's per-step constants. Attack is a linear rise; Decay and Release are exponential, 60 dB in their time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AdsrParams {
    /// The rise per step during Attack.
    attack_step: f32,
    /// The factor per step of the distance to Sustain during Decay.
    decay_coef: f32,
    pub sustain: f32,
    /// The factor per step of the level during Release.
    release_coef: f32,
}

impl AdsrParams {
    /// From the four normalized params (Attack, Decay, Sustain, Release), for an envelope stepped `rate` times a
    /// second.
    pub fn new(attack: f32, decay: f32, sustain: f32, release: f32, rate: f32) -> Self {
        Self::from_seconds(
            envelope_seconds(attack),
            envelope_seconds(decay),
            unit(sustain),
            envelope_seconds(release),
            rate,
        )
    }

    /// From times in seconds and a Sustain level in 0..=1. A segment lasts at least one step.
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
    /// Falling to Sustain.
    Decay,
    /// Holding at Sustain, which follows the param live.
    Sustain,
    Release,
}

/// One envelope's state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Envelope {
    level: f32,
    stage: Stage,
}

impl Envelope {
    /// A key went down: Attack from where the level is now, so a retriggered voice does not click.
    pub fn gate_on(&mut self) {
        self.stage = Stage::Attack;
    }

    /// The key came up: Release from the current level (an idle envelope stays idle).
    pub fn gate_off(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    /// Silence at once.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn level(&self) -> f32 {
        self.level
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// Finished: the level is 0 and nothing will move it until the next [`Self::gate_on`].
    pub fn is_idle(&self) -> bool {
        self.stage == Stage::Idle
    }

    /// The key is still down (Attack, Decay or Sustain).
    pub fn is_held(&self) -> bool {
        matches!(
            self.stage,
            Stage::Attack | Stage::Decay | Stage::Sustain
        )
    }

    /// One step; returns the new level.
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
//
// The LFO's shape is a curve of 2..=[`MAX_POINTS`] points `(x, y)`, x the position in the cycle and y the level (both
// 0..=1; the LFO puts out `2y − 1`), with a *bend* on each segment between two points. The curve lives in the
// instrument's ordinary params ([`CURVE_PARAMS`] of them, laid out below), so it reaches the audio thread the way every
// other param does: the engine calls `set_parameter` there, and nothing here locks or allocates.
//
//   offset 0                         Points: 2..=12, [`choice`]-coded
//   offset 1 + 2i, 2 + 2i            point i's x and y
//   offset 1 + 2·MAX_POINTS + i      segment i's bend, 0.5 = straight
//
// The instrument keeps an [`LfoTable`] (257 floats, on its own stack frame) and a dirty flag. A write to a curve param
// sets the flag; at the next control tick the instrument rebuilds the table from the params
// (`LfoTable::rebuild(&LfoCurve::from_params(..))`, a few hundred multiplications), so a finger drag that sends a
// hundred writes a second costs one rebuild per tick. The UI draws the very same curve with [`lfo_preview`] or edits it
// through [`LfoCurve`], then writes the params back with [`LfoCurve::to_params`].

/// Most points a curve can hold.
pub const MAX_POINTS: usize = 12;
/// Fewest: the two ends.
pub const MIN_POINTS: usize = 2;
/// Segments between [`MAX_POINTS`] points.
pub const SEGMENTS: usize = MAX_POINTS - 1;
/// Params of a curve: the point count, an x and y per point, a bend per segment.
pub const CURVE_PARAMS: usize = 1 + 2 * MAX_POINTS + SEGMENTS;
/// Cells in an [`LfoTable`], not counting the guard cell at x = 1.
pub const TABLE_LEN: usize = 256;

/// The offset of point `i`'s x within a curve's params.
pub const fn x_param(i: usize) -> usize {
    1 + 2 * i
}

/// The offset of point `i`'s y.
pub const fn y_param(i: usize) -> usize {
    2 + 2 * i
}

/// The offset of segment `i`'s bend.
pub const fn bend_param(i: usize) -> usize {
    1 + 2 * MAX_POINTS + i
}

/// A segment's progress `t` (0..=1) after its bend `k` (−1..=1). A positive bend bulges the segment *upward*, a
/// negative one downward, whichever way it runs: a rising segment takes `t^(1/E)` and a falling one `t^E`, with
/// E = 8^k (1/8 … 8). Dragging a segment's handle up is always a positive bend.
fn bend_t(t: f32, k: f32, rising: bool) -> f32 {
    if k == 0.0 {
        return t;
    }
    let e = 8f32.powf(k);
    if rising { t.powf(1.0 / e) } else { t.powf(e) }
}

/// A curve: its points, left to right, and the bends of the segments between them. The first point is at x = 0 and the
/// last at x = 1, always; a point can not pass its neighbours (two at one x make a vertical edge). Every constructor
/// and [`Self::from_params`] enforce that, so any params make a valid curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoCurve {
    count: usize,
    x: [f32; MAX_POINTS],
    y: [f32; MAX_POINTS],
    /// −1..=1 per segment; entries past `count − 1` are 0.
    bend: [f32; SEGMENTS],
}

impl Default for LfoCurve {
    fn default() -> Self {
        CurvePreset::Triangle.curve()
    }
}

impl LfoCurve {
    /// A curve through `points` (x, y) with `bends` between them (a short `bends` leaves the rest straight). More than
    /// [`MAX_POINTS`] points are cut; fewer than two make a flat line at 0.
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

    /// From a curve's [`CURVE_PARAMS`] normalized params; a shorter slice gives the default curve.
    #[allow(clippy::needless_range_loop)] // the arrays are indexed in step
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

    /// The curve as its params, for the UI to write back (point and segment slots past the end hold neutral values).
    #[allow(clippy::needless_range_loop)] // the arrays are indexed in step
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

    /// Makes any field values a valid curve: the count in range, y in 0..=1, the ends at x = 0 and 1, x never
    /// decreasing, bends in −1..=1, everything past the end zeroed (so two equal curves compare equal).
    #[allow(clippy::needless_range_loop)] // the arrays are indexed in step
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

    /// How many points.
    #[allow(clippy::len_without_is_empty)] // a curve is never empty: it has its two ends
    pub fn len(&self) -> usize {
        self.count
    }

    /// Point `i`'s (x, y).
    pub fn point(&self, i: usize) -> (f32, f32) {
        let i = i.min(self.count - 1);
        (self.x[i], self.y[i])
    }

    /// Segment `i`'s bend, −1..=1 (0 for a segment that is not there).
    pub fn bend(&self, i: usize) -> f32 {
        if i + 1 < self.count { self.bend[i] } else { 0.0 }
    }

    /// The curve's level at `x` (0..=1, clamped; NaN reads as 0), 0..=1. At a vertical edge it takes the point after.
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
        // The loop stopped with x[i] <= x < x[i + 1], so the width is positive.
        let t = ((x - self.x[i]) / (self.x[i + 1] - self.x[i])).clamp(0.0, 1.0);
        let (y0, y1) = (self.y[i], self.y[i + 1]);
        y0 + (y1 - y0) * bend_t(t, self.bend[i], y1 >= y0)
    }

    /// Adds a point at `(x, y)` between its neighbours and returns its index; `None` when the curve is full. Pass
    /// `y = self.value(x)` to split a segment without changing the line (both halves keep the segment's bend).
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

    /// Removes point `i`, joining its two segments (the left one's bend stays). The ends and a curve of two points
    /// stay; returns whether anything was removed.
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

    /// Moves point `i` to `(x, y)`: y clamps to 0..=1, an inner point's x between its neighbours', and an end keeps its
    /// x (0 or 1) and takes only the y.
    pub fn move_point(&mut self, i: usize, x: f32, y: f32) {
        if i >= self.count {
            return;
        }
        self.y[i] = unit(y);
        if i > 0 && i < self.count - 1 {
            self.x[i] = unit(x).clamp(self.x[i - 1], self.x[i + 1]);
        }
    }

    /// Sets segment `i`'s bend (−1..=1).
    pub fn set_bend(&mut self, i: usize, k: f32) {
        if i + 1 < self.count && k.is_finite() {
            self.bend[i] = k.clamp(-1.0, 1.0);
        }
    }
}

/// The shapes the UI's buttons load. Each is a curve like any other, so it can be edited from there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurvePreset {
    /// Up from the middle to the top, down to the bottom, back to the middle.
    Triangle,
    /// A rising ramp, −1 → 1, then a jump back.
    Saw,
    /// A falling ramp, 1 → −1, then a jump back.
    Ramp,
    /// Starts at the top and falls fast, then slowly: an envelope-like decay per cycle.
    Pluck,
    /// Five points and four bent segments that approximate a sine to within a tenth.
    Sine,
    /// High for the first half, low for the second, with vertical edges.
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

/// A curve sampled for the audio thread: [`TABLE_LEN`] cells over one cycle plus a guard cell holding the curve at
/// x = 1, so reading between the last two cells ends on the last point and the jump back to the first happens at the
/// wrap, as drawn. Cells are the LFO's output, −1..=1.
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

    /// Resamples `curve` into the table, in place: no allocation, so the audio thread may call it.
    pub fn rebuild(&mut self, curve: &LfoCurve) {
        for (k, cell) in self.values.iter_mut().enumerate() {
            *cell = 2.0 * curve.value(k as f32 / TABLE_LEN as f32) - 1.0;
        }
    }

    /// The output at `phase` (cycles, taken mod 1), linearly interpolated between cells. A vertical edge of the curve
    /// is therefore a slope one cell (1/256 of the cycle) wide.
    pub fn value(&self, phase: f64) -> f32 {
        if !phase.is_finite() {
            return self.values[0];
        }
        let position = phase.rem_euclid(1.0) * TABLE_LEN as f64;
        let i = (position as usize).min(TABLE_LEN - 1);
        let t = (position - i as f64) as f32;
        let a = self.values[i];
        a + (self.values[i + 1] - a) * t
    }
}

/// One cycle of the curve in `params` (a curve's [`CURVE_PARAMS`] params) into `out`, for the UI's canvas: `out[k]` is
/// the LFO's output at phase k / `out.len()`, −1..=1. The same code the audio thread runs, on no shared state.
pub fn lfo_preview(params: &[f32], out: &mut [f32]) {
    let table = LfoTable::from_curve(&LfoCurve::from_params(params));
    let n = out.len();
    for (k, y) in out.iter_mut().enumerate() {
        *y = table.value(k as f64 / n as f64);
    }
}

/// The LFO's settings for one control tick, from the params, the tempo and the song position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LfoSettings {
    /// Tempo-synced (Mode ≥ 0.5) rather than free in Hertz.
    pub sync: bool,
    /// Cycles per control tick.
    pub step: f64,
    /// The phase a note starts the LFO at: 0 in Hertz mode, the song's own phase when synced, so a note lands on the
    /// grid.
    pub start_phase: f64,
}

impl LfoSettings {
    /// From the normalized Rate, Mode and Sync Beat params. `tick_frames` is the control tick's length;
    /// `song_beat` the song position of the tick's first frame. The Hertz rate is the shared
    /// [`lfo_rate_hz`] law, the synced period one entry of [`LFO_SYNC_BEATS`] (1/8 … 32 beats).
    pub fn new(
        rate: f32,
        mode: f32,
        beat: f32,
        tempo_bpm: f32,
        song_beat: f64,
        sample_rate: f32,
        tick_frames: usize,
    ) -> Self {
        let tick_seconds = tick_frames as f64 / f64::from(sample_rate.max(1.0));
        let sync = unit(mode) >= 0.5;
        let (step, start_phase) = if sync {
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
            (f64::from(lfo_rate_hz(unit(rate))) * tick_seconds, 0.0)
        };
        Self {
            sync,
            // Past half a cycle per tick the LFO would only alias.
            step: step.min(0.5),
            start_phase,
        }
    }
}

/// One voice's LFO phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lfo {
    phase: f64,
}

impl Lfo {
    /// Restarts at `phase` (cycles): a note on.
    pub fn start(&mut self, phase: f64) {
        self.phase = phase.rem_euclid(1.0);
    }

    pub fn phase(&self) -> f64 {
        self.phase
    }

    /// The output at the current phase on `table`, −1..=1.
    pub fn value(&self, table: &LfoTable) -> f32 {
        table.value(self.phase)
    }

    /// One control tick on.
    pub fn advance(&mut self, step: f64) {
        self.phase = (self.phase + step).rem_euclid(1.0);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Matrix
// ---------------------------------------------------------------------------------------------------------------------

/// Where a matrix slot takes its signal from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModSource {
    None,
    /// −1..=1.
    Lfo,
    /// 0..=1.
    Env1,
    /// 0..=1.
    Env2,
    /// 0..=1.
    Velocity,
    /// The note's key: −1 at key 0, 0 at middle C (60), +1 at key 120 and up.
    Key,
    /// MIDI CC #1, 0..=1.
    ModWheel,
}

pub const SOURCE_COUNT: usize = 7;

impl ModSource {
    pub const ALL: [ModSource; SOURCE_COUNT] = [
        Self::None,
        Self::Lfo,
        Self::Env1,
        Self::Env2,
        Self::Velocity,
        Self::Key,
        Self::ModWheel,
    ];
    pub const NAMES: [&'static str; SOURCE_COUNT] =
        ["None", "LFO 1", "Env 1", "Env 2", "Velocity", "Key", "Mod Wheel"];

    pub fn from_param(v: f32) -> Self {
        Self::ALL[choice(v, SOURCE_COUNT)]
    }
}

/// What a matrix slot moves.
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
}

pub const DEST_COUNT: usize = 11;

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
    ];

    pub fn from_param(v: f32) -> Self {
        Self::ALL[choice(v, DEST_COUNT)]
    }

    /// What full depth (+1) at a source of 1 adds, in the destination's own unit: Cutoff in octaves, the Pitches in
    /// semitones, the rest as a change of the destination's normalized 0..=1 param.
    pub fn scale(self) -> f32 {
        match self {
            Self::None => 0.0,
            Self::Cutoff => 5.0,
            Self::OscAPitch | Self::OscBPitch => 12.0,
            _ => 1.0,
        }
    }
}

/// The sources' present values, gathered once per control tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModInputs {
    /// −1..=1.
    pub lfo: f32,
    pub env1: f32,
    pub env2: f32,
    pub velocity: f32,
    /// [`ModSource::Key`]'s value.
    pub key: f32,
    pub wheel: f32,
}

impl ModInputs {
    /// The key source's value for MIDI note `note` (0..=127), or a gliding pitch.
    pub fn key_value(note: f32) -> f32 {
        ((note - 60.0) / 60.0).clamp(-1.0, 1.0)
    }

    pub fn source(&self, source: ModSource) -> f32 {
        match source {
            ModSource::None => 0.0,
            ModSource::Lfo => self.lfo,
            ModSource::Env1 => self.env1,
            ModSource::Env2 => self.env2,
            ModSource::Velocity => self.velocity,
            ModSource::Key => self.key,
            ModSource::ModWheel => self.wheel,
        }
    }
}

/// One matrix slot: `source` × `depth` added to `dest`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub source: ModSource,
    pub dest: ModDest,
    /// −1..=1: the Depth param's `2v − 1`, so the centre of the knob is 0.
    pub depth: f32,
}

impl Slot {
    /// A slot that does nothing.
    pub const OFF: Slot = Slot {
        source: ModSource::None,
        dest: ModDest::None,
        depth: 0.0,
    };

    /// From the slot's three normalized params: Source, Destination, Depth.
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

/// Slots in the matrix.
pub const SLOTS: usize = 4;
/// Params per slot: Source, Destination, Depth.
pub const SLOT_PARAMS: usize = 3;

/// The modulation each destination receives this tick, in its own unit ([`ModDest::scale`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModValues([f32; DEST_COUNT]);

impl ModValues {
    pub fn get(&self, dest: ModDest) -> f32 {
        self.0[dest as usize]
    }
}

/// The four slots.
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
    /// From the matrix's params in order, slot after slot ([`SLOT_PARAMS`] each). A short slice leaves the rest of the
    /// slots off.
    pub fn from_params(params: &[f32]) -> Self {
        let mut matrix = Self::default();
        for (slot, p) in matrix.slots.iter_mut().zip(params.chunks_exact(SLOT_PARAMS)) {
            *slot = Slot::from_params(p[0], p[1], p[2]);
        }
        matrix
    }

    /// Every slot's contribution summed per destination. The caller applies each sum to its destination and clamps
    /// there (a WT Pos to 0..=1, a Cutoff to the filter's range, and so on). A slot with no source, no destination or
    /// zero depth does nothing; two slots on one destination add.
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

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 1000.0;

    #[test]
    fn menu_values_round_trip() {
        for count in [2, 4, SOURCE_COUNT, DEST_COUNT] {
            for index in 0..count {
                assert_eq!(choice(menu_value(index, count), count), index, "{index}/{count}");
            }
        }
        assert_eq!(choice(f32::NAN, 7), 0);
        assert_eq!(choice(9.0, 7), 6);
        assert_eq!(choice(-1.0, 7), 0);
        assert_eq!(ModSource::from_param(0.0), ModSource::None);
        assert_eq!(ModSource::from_param(1.0), ModSource::ModWheel);
        assert_eq!(ModDest::from_param(1.0), ModDest::OscBDetune);
    }

    #[test]
    fn envelope_time_runs_from_a_millisecond_to_ten_seconds() {
        assert!((envelope_seconds(0.0) - 0.001).abs() < 1e-7);
        assert!((envelope_seconds(1.0) - 10.0).abs() < 1e-3);
        assert!((envelope_seconds(0.5) - 0.1).abs() < 1e-4);
        assert_eq!(envelope_seconds(f32::NAN), envelope_seconds(0.0));
    }

    #[test]
    fn attack_rises_linearly_then_decay_settles_on_sustain() {
        let params = AdsrParams::from_seconds(0.1, 0.1, 0.5, 0.1, RATE);
        let mut env = Envelope::default();
        env.gate_on();
        for _ in 0..50 {
            env.advance(&params);
        }
        assert!((env.level() - 0.5).abs() < 0.01, "{}", env.level());
        assert_eq!(env.stage(), Stage::Attack);
        // Float steps may land a hair under 1, so give the top a few steps' room.
        for _ in 0..55 {
            env.advance(&params);
        }
        assert_eq!(env.stage(), Stage::Decay);
        assert!(env.level() > 0.8 && env.level() <= 1.0, "{}", env.level());
        // 60 dB of the 0.5 distance in 100 steps, under the snap threshold by about step 123.
        for _ in 0..150 {
            env.advance(&params);
        }
        assert_eq!(env.stage(), Stage::Sustain);
        assert_eq!(env.level(), 0.5);
        assert!(env.is_held());
        // Sustain follows the param while held.
        let lower = AdsrParams::from_seconds(0.1, 0.1, 0.25, 0.1, RATE);
        env.advance(&lower);
        assert_eq!(env.level(), 0.25);
    }

    #[test]
    fn release_falls_60_db_in_its_time_and_ends_idle() {
        let params = AdsrParams::from_seconds(0.001, 0.001, 0.5, 0.1, RATE);
        let mut env = Envelope::default();
        env.gate_on();
        for _ in 0..20 {
            env.advance(&params);
        }
        assert_eq!(env.level(), 0.5);
        env.gate_off();
        assert!(!env.is_held());
        for _ in 0..100 {
            env.advance(&params);
        }
        assert!((env.level() - 0.5e-3).abs() < 2e-5, "{}", env.level());
        for _ in 0..30 {
            env.advance(&params);
        }
        assert!(env.is_idle());
        assert_eq!(env.level(), 0.0);
        // An idle envelope stays idle and silent.
        assert_eq!(env.advance(&params), 0.0);
        env.gate_off();
        assert!(env.is_idle());
    }

    #[test]
    fn a_retrigger_attacks_from_the_current_level() {
        let params = AdsrParams::from_seconds(0.1, 0.1, 0.5, 0.1, RATE);
        let mut env = Envelope::default();
        env.gate_on();
        for _ in 0..100 {
            env.advance(&params);
        }
        env.gate_off();
        for _ in 0..10 {
            env.advance(&params);
        }
        let before = env.level();
        assert!(before > 0.0 && before < 1.0);
        env.gate_on();
        let after = env.advance(&params);
        assert!(after > before && after <= before + 0.011, "{before} {after}");
        env.reset();
        assert!(env.is_idle() && env.level() == 0.0);
    }

    #[test]
    fn a_zero_time_segment_still_takes_one_step() {
        let params = AdsrParams::from_seconds(0.0, 0.0, 0.3, 0.0, RATE);
        let mut env = Envelope::default();
        env.gate_on();
        assert_eq!(env.advance(&params), 1.0);
        for _ in 0..3 {
            env.advance(&params);
        }
        assert_eq!(env.level(), 0.3);
        env.gate_off();
        env.advance(&params);
        env.advance(&params);
        assert!(env.is_idle());
    }

    #[test]
    fn hertz_mode_steps_by_the_shared_rate_law() {
        let s = LfoSettings::new(0.5, 0.0, 0.0, 120.0, 3.0, 48_000.0, 48);
        assert!(!s.sync);
        assert_eq!(s.start_phase, 0.0);
        assert!((s.step - f64::from(lfo_rate_hz(0.5)) * 0.001).abs() < 1e-9);
    }

    #[test]
    fn sync_mode_locks_the_step_to_the_tempo_and_the_start_to_the_song() {
        // Beat 0 is 1/8 of a beat; at 120 BPM that is 16 Hz, 0.016 cycles per millisecond tick.
        let s = LfoSettings::new(0.0, 1.0, 0.0, 120.0, 0.0625, 48_000.0, 48);
        assert!(s.sync);
        assert!((s.step - 0.016).abs() < 1e-9, "{}", s.step);
        assert!((s.start_phase - 0.5).abs() < 1e-9);
        // The slowest, 32 beats, at 60 BPM: 1/32 Hz.
        let s = LfoSettings::new(0.0, 1.0, 1.0, 60.0, 16.0, 48_000.0, 48);
        assert!((s.step - 0.001 / 32.0).abs() < 1e-9);
        assert!((s.start_phase - 0.5).abs() < 1e-9);
        // A bad tempo falls back to 120.
        let s = LfoSettings::new(0.0, 1.0, 0.0, f32::NAN, 0.0, 48_000.0, 48);
        assert!((s.step - 0.016).abs() < 1e-9);
        // The mode switches at one half.
        assert!(!LfoSettings::new(0.0, 0.49, 0.0, 120.0, 0.0, 48_000.0, 48).sync);
        assert!(LfoSettings::new(0.0, 0.5, 0.0, 120.0, 0.0, 48_000.0, 48).sync);
    }

    #[test]
    fn an_lfo_runs_whole_cycles_and_wraps() {
        let mut lfo = Lfo::default();
        lfo.start(0.75);
        assert_eq!(lfo.phase(), 0.75);
        for _ in 0..8 {
            lfo.advance(0.125);
        }
        assert!((lfo.phase() - 0.75).abs() < 1e-12);
        lfo.start(2.5);
        assert!((lfo.phase() - 0.5).abs() < 1e-12);
        // Phase 0.5 on the square preset is the first low cell.
        let square = LfoTable::from_curve(&CurvePreset::Square.curve());
        assert_eq!(lfo.value(&square), -1.0);
        lfo.start(0.25);
        assert_eq!(lfo.value(&square), 1.0);
    }

    #[test]
    fn a_slot_scales_its_source_by_depth_and_the_destination_unit() {
        let inputs = ModInputs {
            lfo: 0.5,
            env2: 1.0,
            ..ModInputs::default()
        };
        // Depth param 1.0 is +1: the LFO's 0.5 on A WT Pos.
        let m = ModMatrix::from_params(&[
            menu_value(1, SOURCE_COUNT),
            menu_value(1, DEST_COUNT),
            1.0,
        ]);
        assert!((m.evaluate(&inputs).get(ModDest::OscAWtPos) - 0.5).abs() < 1e-6);
        // Env 2 at depth +0.5 on Cutoff: 1 × 0.5 × 5 octaves.
        let m = ModMatrix::from_params(&[
            menu_value(3, SOURCE_COUNT),
            menu_value(3, DEST_COUNT),
            0.75,
        ]);
        assert!((m.evaluate(&inputs).get(ModDest::Cutoff) - 2.5).abs() < 1e-6);
        // Negative depth inverts; a Pitch destination is in semitones.
        let m = ModMatrix::from_params(&[
            menu_value(3, SOURCE_COUNT),
            menu_value(5, DEST_COUNT),
            0.0,
        ]);
        assert!((m.evaluate(&inputs).get(ModDest::OscAPitch) + 12.0).abs() < 1e-5);
    }

    #[test]
    fn slots_add_and_idle_slots_do_nothing() {
        let inputs = ModInputs {
            lfo: 1.0,
            env1: 0.5,
            velocity: 1.0,
            ..ModInputs::default()
        };
        let slot = |source, dest, depth| Slot {
            source,
            dest,
            depth,
        };
        let m = ModMatrix {
            slots: [
                slot(ModSource::Lfo, ModDest::OscAWtPos, 0.25),
                slot(ModSource::Env1, ModDest::OscAWtPos, 0.5),
                // No source, no destination, zero depth: none of these count.
                slot(ModSource::None, ModDest::Cutoff, 1.0),
                slot(ModSource::Velocity, ModDest::None, 1.0),
            ],
        };
        let v = m.evaluate(&inputs);
        assert!((v.get(ModDest::OscAWtPos) - 0.5).abs() < 1e-6);
        assert_eq!(v.get(ModDest::Cutoff), 0.0);
        assert_eq!(v.get(ModDest::None), 0.0);
        let off = slot(ModSource::Lfo, ModDest::Cutoff, 0.0);
        assert_eq!(ModMatrix { slots: [off; SLOTS] }.evaluate(&inputs), ModValues::default());
        // A centred Depth knob is no modulation.
        let centred = Slot::from_params(menu_value(1, SOURCE_COUNT), menu_value(3, DEST_COUNT), 0.5);
        assert_eq!(centred.depth, 0.0);
    }

    #[test]
    fn a_short_param_slice_leaves_the_rest_off_and_nan_is_harmless() {
        let m = ModMatrix::from_params(&[0.5; 4]);
        assert_eq!(m.slots[1], Slot::OFF);
        assert_eq!(ModMatrix::from_params(&[]), ModMatrix::default());
        let nan = ModInputs {
            lfo: f32::NAN,
            ..ModInputs::default()
        };
        let m = ModMatrix::from_params(&[
            menu_value(1, SOURCE_COUNT),
            menu_value(3, DEST_COUNT),
            1.0,
        ]);
        assert_eq!(m.evaluate(&nan).get(ModDest::Cutoff), 0.0);
        assert_eq!(Slot::from_params(f32::NAN, f32::NAN, f32::NAN), Slot::OFF);
    }

    #[test]
    fn the_key_source_is_zero_at_middle_c() {
        assert_eq!(ModInputs::key_value(60.0), 0.0);
        assert!((ModInputs::key_value(72.0) - 0.2).abs() < 1e-6);
        assert_eq!(ModInputs::key_value(0.0), -1.0);
        assert_eq!(ModInputs::key_value(127.0), 1.0);
    }

    // ---- the curve LFO -------------------------------------------------------------------------------------------

    fn table(preset: CurvePreset) -> LfoTable {
        LfoTable::from_curve(&preset.curve())
    }

    #[test]
    fn the_presets_draw_what_they_name() {
        let tri = table(CurvePreset::Triangle);
        assert!(tri.value(0.0).abs() < 1e-6);
        assert!((tri.value(0.25) - 1.0).abs() < 1e-6);
        assert!(tri.value(0.5).abs() < 1e-6);
        assert!((tri.value(0.75) + 1.0).abs() < 1e-6);
        let saw = table(CurvePreset::Saw);
        assert!((saw.value(0.0) + 1.0).abs() < 1e-6);
        assert!(saw.value(0.5).abs() < 1e-6);
        assert!(saw.value(0.999) > 0.99);
        let ramp = table(CurvePreset::Ramp);
        assert!((ramp.value(0.0) - 1.0).abs() < 1e-6);
        assert!(ramp.value(0.999) < -0.99);
        // The pluck falls faster than the straight ramp, and keeps falling.
        let pluck = table(CurvePreset::Pluck);
        assert!(pluck.value(0.1) < ramp.value(0.1) - 0.2);
        let mut last = f32::MAX;
        for k in 0..100 {
            let v = pluck.value(k as f64 / 100.0);
            assert!(v <= last + 1e-6);
            last = v;
        }
        let square = table(CurvePreset::Square);
        assert_eq!(square.value(0.0), 1.0);
        assert_eq!(square.value(0.25), 1.0);
        assert_eq!(square.value(0.75), -1.0);
        assert_eq!(square.value(0.99), -1.0);
        // A vertical edge is a slope one cell wide.
        assert!(square.value(0.5 - 0.5 / 256.0).abs() < 1e-5);
    }

    #[test]
    fn the_sine_preset_stays_within_a_tenth_of_a_sine() {
        let sine = table(CurvePreset::Sine);
        for k in 0..64 {
            let phase = k as f64 / 64.0;
            let want = (std::f64::consts::TAU * phase).sin() as f32;
            assert!((sine.value(phase) - want).abs() < 0.1, "phase {phase}");
        }
    }

    #[test]
    fn a_positive_bend_bulges_up_whichever_way_the_segment_runs() {
        for (points, up) in [
            ([(0.0, 0.0), (1.0, 1.0)], true),
            ([(0.0, 1.0), (1.0, 0.0)], true),
            ([(0.0, 0.0), (1.0, 1.0)], false),
            ([(0.0, 1.0), (1.0, 0.0)], false),
        ] {
            let k = if up { 0.5 } else { -0.5 };
            let curve = LfoCurve::from_points(&points, &[k]);
            let chord = 0.5;
            let mid = curve.value(0.5);
            assert!(if up { mid > chord + 0.1 } else { mid < chord - 0.1 }, "{points:?} {k}: {mid}");
            assert_eq!(curve.value(0.0), points[0].1);
            assert_eq!(curve.value(1.0), points[1].1);
        }
        // No bend, no bulge.
        let straight = LfoCurve::from_points(&[(0.0, 0.0), (1.0, 1.0)], &[0.0]);
        assert_eq!(straight.value(0.5), 0.5);
        // A flat segment ignores its bend.
        let flat = LfoCurve::from_points(&[(0.0, 0.3), (1.0, 0.3)], &[1.0]);
        assert_eq!(flat.value(0.7), 0.3);
    }

    #[test]
    fn the_table_ends_on_the_last_point_and_wraps_to_the_first() {
        // A saw with a different last y: the guard cell holds the curve at x = 1, the first cell the curve at 0.
        let curve = LfoCurve::from_points(&[(0.0, 0.0), (1.0, 0.8)], &[]);
        let table = LfoTable::from_curve(&curve);
        assert!((table.value(0.0) + 1.0).abs() < 1e-6);
        assert!((table.value(1.0 - 1e-9) - 0.6).abs() < 1e-3);
        assert!((table.value(1.25) - table.value(0.25)).abs() < 1e-6);
        assert!((table.value(-0.75) - table.value(0.25)).abs() < 1e-6);
        assert_eq!(table.value(f64::NAN), table.value(0.0));
    }

    #[test]
    fn params_round_trip_for_every_preset() {
        for preset in CurvePreset::ALL {
            let curve = preset.curve();
            let params = curve.to_params();
            assert_eq!(params.len(), CURVE_PARAMS);
            assert!(params.iter().all(|p| (0.0..=1.0).contains(p)), "{preset:?}");
            assert_eq!(LfoCurve::from_params(&params), curve, "{preset:?}");
        }
        assert_eq!(LfoCurve::default(), CurvePreset::Triangle.curve());
        assert_eq!(LfoCurve::from_params(&[0.5; 3]), LfoCurve::default());
    }

    #[test]
    fn the_point_count_param_runs_from_two_to_twelve() {
        let mut params = CurvePreset::Triangle.curve().to_params();
        params[0] = 0.0;
        assert_eq!(LfoCurve::from_params(&params).len(), MIN_POINTS);
        params[0] = 1.0;
        assert_eq!(LfoCurve::from_params(&params).len(), MAX_POINTS);
        params[0] = f32::NAN;
        assert_eq!(LfoCurve::from_params(&params).len(), MIN_POINTS);
        assert_eq!(CURVE_PARAMS, 36);
        assert_eq!((x_param(0), y_param(0), bend_param(0)), (1, 2, 25));
        assert_eq!(bend_param(SEGMENTS - 1), CURVE_PARAMS - 1);
    }

    #[test]
    fn any_params_make_a_valid_curve() {
        // x going backwards is pushed, the ends are pinned, y is clamped, NaN is harmless.
        let mut params = CurvePreset::Triangle.curve().to_params();
        params[0] = menu_value(2, 11); // four points
        params[x_param(0)] = 0.4;
        params[x_param(1)] = 0.8;
        params[x_param(2)] = 0.3;
        params[x_param(3)] = 0.2;
        params[y_param(1)] = 7.0;
        params[y_param(2)] = f32::NAN;
        let curve = LfoCurve::from_params(&params);
        assert_eq!(curve.point(0), (0.0, 0.5));
        assert_eq!(curve.point(1), (0.8, 1.0));
        assert_eq!(curve.point(2), (0.8, 0.0));
        assert_eq!(curve.point(3).0, 1.0);
        for k in 0..=256 {
            let v = curve.value(k as f32 / 256.0);
            assert!((0.0..=1.0).contains(&v));
        }
        // Wildly out of range bends clamp.
        params[bend_param(0)] = 40.0;
        assert_eq!(LfoCurve::from_params(&params).bend(0), 1.0);
        params[bend_param(0)] = -40.0;
        assert_eq!(LfoCurve::from_params(&params).bend(0), -1.0);
    }

    #[test]
    fn inserting_on_the_curve_keeps_the_line_and_a_full_curve_refuses() {
        let mut curve = CurvePreset::Triangle.curve();
        let before: Vec<f32> = (0..=64).map(|k| curve.value(k as f32 / 64.0)).collect();
        let y = curve.value(0.5);
        assert_eq!(curve.insert(0.5, y), Some(2));
        assert_eq!(curve.len(), 5);
        assert_eq!(curve.point(2), (0.5, y));
        for (k, want) in before.iter().enumerate() {
            assert!((curve.value(k as f32 / 64.0) - want).abs() < 1e-6, "k {k}");
        }
        // Before the first inner point, and at the very end.
        assert_eq!(curve.insert(0.1, curve.value(0.1)), Some(1));
        let at_end = curve.insert(1.0, 0.2).unwrap();
        assert_eq!(at_end, curve.len() - 2);
        assert_eq!(curve.point(curve.len() - 1).0, 1.0);
        while curve.len() < MAX_POINTS {
            assert!(curve.insert(0.6, 0.5).is_some());
        }
        assert_eq!(curve.insert(0.6, 0.5), None);
        // Still a valid curve: x never decreases.
        for i in 1..curve.len() {
            assert!(curve.point(i).0 >= curve.point(i - 1).0);
        }
    }

    #[test]
    fn a_split_segment_keeps_its_bend_on_both_halves() {
        let mut curve = CurvePreset::Pluck.curve();
        let y = curve.value(0.5);
        curve.insert(0.5, y);
        assert_eq!(curve.bend(0), -0.5);
        assert_eq!(curve.bend(1), -0.5);
        assert_eq!(curve.bend(2), 0.0);
        assert!(curve.remove(1));
        assert_eq!(curve.len(), 2);
        assert_eq!(curve, CurvePreset::Pluck.curve());
    }

    #[test]
    fn the_ends_and_the_last_two_points_can_not_be_removed() {
        let mut curve = CurvePreset::Triangle.curve();
        assert!(!curve.remove(0));
        assert!(!curve.remove(3));
        assert!(!curve.remove(9));
        assert!(curve.remove(1));
        assert!(curve.remove(1));
        assert_eq!(curve.len(), 2);
        assert!(!curve.remove(1));
        assert_eq!(curve.point(0), (0.0, 0.5));
        assert_eq!(curve.point(1), (1.0, 0.5));
    }

    #[test]
    fn a_dragged_point_stays_between_its_neighbours() {
        let mut curve = CurvePreset::Triangle.curve();
        curve.move_point(1, 0.9, 2.0);
        assert_eq!(curve.point(1), (0.75, 1.0));
        curve.move_point(2, 0.0, -1.0);
        assert_eq!(curve.point(2), (0.75, 0.0));
        curve.move_point(1, 0.5, 0.6);
        assert_eq!(curve.point(1), (0.5, 0.6));
        // An end moves only in y.
        curve.move_point(0, 0.5, 0.2);
        curve.move_point(3, 0.2, 0.9);
        assert_eq!(curve.point(0), (0.0, 0.2));
        assert_eq!(curve.point(3), (1.0, 0.9));
        // Out of range, and a bend on a segment that is not there, do nothing.
        let copy = curve;
        curve.move_point(7, 0.5, 0.5);
        curve.set_bend(3, 1.0);
        curve.set_bend(0, f32::NAN);
        assert_eq!(curve, copy);
        curve.set_bend(0, 9.0);
        assert_eq!(curve.bend(0), 1.0);
    }

    #[test]
    fn the_preview_is_the_audio_curve() {
        let params = CurvePreset::Sine.curve().to_params();
        let mut out = [0.0f32; 64];
        lfo_preview(&params, &mut out);
        let table = table(CurvePreset::Sine);
        for (k, y) in out.iter().enumerate() {
            assert_eq!(*y, table.value(k as f64 / 64.0));
        }
        assert!((out[16] - 1.0).abs() < 1e-6);
        lfo_preview(&params, &mut []);
    }

    #[test]
    fn rebuilding_a_table_in_place_takes_the_new_curve() {
        let mut table = LfoTable::default();
        assert!((table.value(0.25) - 1.0).abs() < 1e-6);
        table.rebuild(&CurvePreset::Ramp.curve());
        assert!((table.value(0.0) - 1.0).abs() < 1e-6);
        assert_eq!(table, LfoTable::from_curve(&CurvePreset::Ramp.curve()));
    }
}
