//! DewSerum's wavetable bank: four single-cycle frames (Sine, Triangle, Saw, Square) that `WT Pos` morphs between, each
//! frame kept in [`MIP_LEVELS`] band-limited copies so a note never reads a harmonic above Nyquist.
//!
//! The bank is built once, on the app thread (in `DewSerum::new`), by additive synthesis, and then only read:
//! the audio thread calls [`WavetableBank::reader`] once per control tick and [`Reader::read`] per sample, neither of
//! which allocates. Pure `std`, so it builds for Android and the web unchanged.
//!
//! Layout: the tables are stored frame-major, `MIP_LEVELS` levels each, `STRIDE` samples per table. A table holds
//! [`FRAME_LEN`] samples plus one guard sample (a copy of the first), so linear interpolation never wraps an index.

use std::f64::consts::{PI, TAU};
use std::sync::OnceLock;

/// Samples in one cycle of a table.
pub const FRAME_LEN: usize = 2048;
/// Frames in the bank: Sine, Triangle, Saw, Square.
pub const FRAMES: usize = 4;
/// The frames' names, in morph order (`WT Pos` 0 → 1).
pub const FRAME_NAMES: [&str; FRAMES] = ["Sine", "Triangle", "Saw", "Square"];
/// Band-limited copies of each frame, one per octave of pitch.
pub const MIP_LEVELS: usize = 10;
/// The most harmonics a level holds: `1023 >> level`, so level 0 stops just below the table's own Nyquist (bin 1024).
pub const HARMONICS: [usize; MIP_LEVELS] = [1023, 511, 255, 127, 63, 31, 15, 7, 3, 1];
/// The level [`wave_preview`] draws: 127 harmonics, enough for a crisp corner without a wall of ripple.
pub const PREVIEW_LEVEL: usize = 3;

/// Samples stored per table: one cycle and the guard sample.
const STRIDE: usize = FRAME_LEN + 1;
/// A 32-bit phase indexes the table with its top bits and interpolates with the rest.
const INDEX_SHIFT: u32 = 32 - FRAME_LEN.trailing_zeros();
const FRAC_MASK: u32 = (1 << INDEX_SHIFT) - 1;
const FRAC_SCALE: f32 = 1.0 / (1u32 << INDEX_SHIFT) as f32;
const PHASE_SCALE: f64 = 4_294_967_296.0;

/// Harmonic `h` (≥ 1) of `frame` as a sine-series amplitude: the frame is Σ amplitude · sin(h·θ).
fn amplitude(frame: usize, h: usize) -> f64 {
    let odd = h % 2 == 1;
    match frame {
        // sin θ.
        0 => {
            if h == 1 {
                1.0
            } else {
                0.0
            }
        }
        // 8/π² · Σ (−1)^k sin((2k+1)θ) / (2k+1)²: peaks at +1 at θ = π/2. For odd h, k = (h − 1) / 2 = h / 2.
        1 if odd => {
            let sign = if (h / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
            8.0 / (PI * PI) * sign / (h * h) as f64
        }
        // 2/π · Σ sin(hθ) / h: a falling saw, as SuperSaw's.
        2 => 2.0 / PI / h as f64,
        // 4/π · Σ sin(hθ) / h over odd h.
        3 if odd => 4.0 / PI / h as f64,
        _ => 0.0,
    }
}

/// The finished bank. Get it with [`bank`].
pub struct WavetableBank {
    data: Box<[f32]>,
}

impl WavetableBank {
    fn build() -> Self {
        let sine: Vec<f64> = (0..FRAME_LEN)
            .map(|n| (TAU * n as f64 / FRAME_LEN as f64).sin())
            .collect();
        let mut data = vec![0.0f32; FRAMES * MIP_LEVELS * STRIDE];
        let mut acc = vec![0.0f64; FRAME_LEN];
        for frame in 0..FRAMES {
            acc.fill(0.0);
            let mut added = 0;
            // Coarsest level first, so each finer one only adds the harmonics between.
            for level in (0..MIP_LEVELS).rev() {
                while added < HARMONICS[level] {
                    added += 1;
                    let a = amplitude(frame, added);
                    if a != 0.0 {
                        for (n, sample) in acc.iter_mut().enumerate() {
                            *sample += a * sine[added * n % FRAME_LEN];
                        }
                    }
                }
                let offset = (frame * MIP_LEVELS + level) * STRIDE;
                let table = &mut data[offset..offset + STRIDE];
                for (out, &sample) in table.iter_mut().zip(&acc) {
                    *out = sample as f32;
                }
                table[FRAME_LEN] = table[0];
            }
            // One scale per frame, from its fullest level: a frame's fundamental keeps one level across the mips, and
            // the fullest copy peaks at 1 (the Gibbs overshoot included).
            let full = (frame * MIP_LEVELS) * STRIDE;
            let peak = data[full..full + FRAME_LEN]
                .iter()
                .fold(0.0f32, |peak, s| peak.max(s.abs()));
            let scale = if peak > 0.0 { 1.0 / peak } else { 1.0 };
            let start = frame * MIP_LEVELS * STRIDE;
            data[start..start + MIP_LEVELS * STRIDE]
                .iter_mut()
                .for_each(|s| *s *= scale);
        }
        Self {
            data: data.into_boxed_slice(),
        }
    }

    /// The table of `frame` at `level`: [`FRAME_LEN`] samples and the guard sample.
    pub fn table(&self, frame: usize, level: usize) -> &[f32] {
        let offset = (frame.min(FRAMES - 1) * MIP_LEVELS + level.min(MIP_LEVELS - 1)) * STRIDE;
        &self.data[offset..offset + STRIDE]
    }

    /// The mip level for a note of `freq` Hz at `sample_rate`: the fullest one whose highest harmonic stays at or
    /// below Nyquist. A pitch whose own fundamental is past Nyquist gets the sine-only level.
    pub fn level_for(freq: f32, sample_rate: f32) -> usize {
        if !(freq.is_finite() && freq > 0.0 && sample_rate.is_finite() && sample_rate > 0.0) {
            return 0;
        }
        let allowed = (0.5 * sample_rate / freq).floor();
        HARMONICS
            .iter()
            .position(|&h| h as f32 <= allowed)
            .unwrap_or(MIP_LEVELS - 1)
    }

    /// A reader of the morph `morph` at `level`, for a stretch of samples (one control tick).
    pub fn reader(&self, level: usize, morph: Morph) -> Reader<'_> {
        Reader {
            a: self.table(morph.lo, level),
            b: self.table(morph.hi, level),
            mix: morph.mix,
        }
    }
}

/// The bank, built on first use. Call it on the app thread (`DewSerum::new` does) so the build never lands on the
/// audio thread; the audio thread only ever finds it built.
pub fn bank() -> &'static WavetableBank {
    static BANK: OnceLock<WavetableBank> = OnceLock::new();
    BANK.get_or_init(WavetableBank::build)
}

/// Where `WT Pos` sits between two neighbouring frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Morph {
    /// The frame at the lower side, and the one above it.
    pub lo: usize,
    pub hi: usize,
    /// 0 = all `lo`, 1 = all `hi`.
    pub mix: f32,
}

impl Morph {
    /// `pos` in 0..=1 spreads evenly over the frames (0 Sine, 1/3 Triangle, 2/3 Saw, 1 Square). Outside the range it
    /// clamps; NaN reads as 0.
    pub fn new(pos: f32) -> Self {
        let pos = if pos.is_finite() {
            pos.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let x = pos * (FRAMES - 1) as f32;
        let lo = (x as usize).min(FRAMES - 2);
        Self {
            lo,
            hi: lo + 1,
            mix: x - lo as f32,
        }
    }
}

/// Two tables and their mix, ready to read by phase. `Copy` and allocation-free: a voice makes one per oscillator
/// voice per control tick and reads it every sample.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    a: &'a [f32],
    b: &'a [f32],
    mix: f32,
}

impl Reader<'_> {
    /// The sample at a 32-bit `phase` (the full `u32` range is one cycle), linearly interpolated within each table and
    /// then between the two.
    #[inline]
    pub fn read(&self, phase: u32) -> f32 {
        let i = (phase >> INDEX_SHIFT) as usize;
        let t = (phase & FRAC_MASK) as f32 * FRAC_SCALE;
        let a = self.a[i] + (self.a[i + 1] - self.a[i]) * t;
        if self.mix == 0.0 {
            return a;
        }
        let b = self.b[i] + (self.b[i + 1] - self.b[i]) * t;
        a + (b - a) * self.mix
    }
}

/// One cycle of the wave at `pos` (WT Pos, 0..=1) into `out`, for the UI's canvas: `out[k]` is the wave at phase
/// k / `out.len()`. It reads the shared bank and touches no voice, so the UI thread can call it freely.
pub fn wave_preview(pos: f32, out: &mut [f32]) {
    let reader = bank().reader(PREVIEW_LEVEL, Morph::new(pos));
    let n = out.len();
    for (k, y) in out.iter_mut().enumerate() {
        *y = reader.read((k as f64 / n as f64 * PHASE_SCALE) as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The magnitude of sine-series bin `k` of a table: (2/N)·|Σ x[n]·sin(2πkn/N)|.
    fn bin(table: &[f32], k: usize) -> f64 {
        let sum: f64 = (0..FRAME_LEN)
            .map(|n| {
                let angle = TAU * (k * n % FRAME_LEN) as f64 / FRAME_LEN as f64;
                f64::from(table[n]) * angle.sin()
            })
            .sum();
        (2.0 / FRAME_LEN as f64 * sum).abs()
    }

    #[test]
    fn every_table_has_a_guard_sample_and_stays_within_unit_peak() {
        let bank = bank();
        for frame in 0..FRAMES {
            for level in 0..MIP_LEVELS {
                let table = bank.table(frame, level);
                assert_eq!(table.len(), FRAME_LEN + 1);
                assert_eq!(table[FRAME_LEN], table[0], "frame {frame} level {level}");
                let peak = table.iter().fold(0.0f32, |p, s| p.max(s.abs()));
                assert!(
                    peak <= 1.0 + 1e-3,
                    "frame {frame} level {level}: peak {peak}"
                );
                assert!(table.iter().all(|s| s.is_finite()));
            }
        }
        // Each frame's fullest copy peaks at exactly 1.
        for frame in 0..FRAMES {
            let peak = bank.table(frame, 0).iter().fold(0.0f32, |p, s| p.max(s.abs()));
            assert!((peak - 1.0).abs() < 1e-4, "frame {frame}: {peak}");
        }
    }

    #[test]
    fn the_sine_frame_is_a_sine_at_every_level() {
        let bank = bank();
        for level in 0..MIP_LEVELS {
            let table = bank.table(0, level);
            for n in (0..FRAME_LEN).step_by(37) {
                let want = (TAU * n as f64 / FRAME_LEN as f64).sin() as f32;
                assert!((table[n] - want).abs() < 1e-5, "level {level} n {n}");
            }
        }
    }

    #[test]
    fn a_level_holds_no_harmonic_above_its_cap_and_has_the_one_at_it() {
        let bank = bank();
        for (frame, level) in [(2, 5), (3, 6), (1, 4), (2, 2)] {
            let table = bank.table(frame, level);
            let cap = HARMONICS[level];
            for k in (cap + 1)..(cap + 40).min(1024) {
                assert!(bin(table, k) < 1e-5, "frame {frame} level {level} bin {k}");
            }
            // The saw has every harmonic, the others only odd ones; the cap is odd for every level.
            assert!(bin(table, cap) > 1e-4, "frame {frame} level {level} at cap");
        }
    }

    #[test]
    fn the_shapes_are_the_ones_named() {
        let bank = bank();
        let quarter = FRAME_LEN / 4;
        // Triangle: 0 at the start, +1 a quarter in, −1 at three quarters.
        let tri = bank.table(1, 0);
        assert!(tri[0].abs() < 1e-4);
        assert!((tri[quarter] - 1.0).abs() < 1e-3);
        assert!((tri[3 * quarter] + 1.0).abs() < 1e-3);
        // Saw falls: high just after the start, low just before the end, one cycle of a falling ramp.
        let saw = bank.table(2, 0);
        assert!(saw[8] > 0.7 && saw[FRAME_LEN - 8] < -0.7);
        assert!(saw[quarter] > saw[2 * quarter] && saw[2 * quarter] > saw[3 * quarter]);
        // Square: positive in the first half, negative in the second.
        let square = bank.table(3, 0);
        assert!(square[quarter] > 0.8 && square[3 * quarter] < -0.8);
    }

    #[test]
    fn morph_positions_land_on_the_frames() {
        assert_eq!(Morph::new(0.0), Morph { lo: 0, hi: 1, mix: 0.0 });
        assert_eq!(Morph::new(1.0), Morph { lo: 2, hi: 3, mix: 1.0 });
        assert_eq!(Morph::new(-5.0), Morph::new(0.0));
        assert_eq!(Morph::new(5.0), Morph::new(1.0));
        assert_eq!(Morph::new(f32::NAN), Morph::new(0.0));
        let m = Morph::new(0.5);
        assert_eq!((m.lo, m.hi), (1, 2));
        assert!((m.mix - 0.5).abs() < 1e-6);
        let m = Morph::new(1.0 / 3.0);
        assert!(m.lo == 1 && m.mix < 1e-3 || m.lo == 0 && m.mix > 0.999, "{m:?}");
    }

    #[test]
    fn reading_morphs_linearly_between_frames_and_wraps_cleanly() {
        let bank = bank();
        let (tri, saw) = (bank.table(1, 2), bank.table(2, 2));
        let reader = bank.reader(2, Morph::new(0.5));
        for n in (0..FRAME_LEN).step_by(101) {
            let phase = (n as u64 * (1 << INDEX_SHIFT)) as u32;
            let want = 0.5 * (tri[n] + saw[n]);
            assert!((reader.read(phase) - want).abs() < 1e-5, "n {n}");
        }
        // Midway between two samples: their mean. The last cell reads into the guard sample.
        let sine = bank.reader(0, Morph::new(0.0));
        let half = 1u32 << (INDEX_SHIFT - 1);
        let t = bank.table(0, 0);
        assert!((sine.read(half) - 0.5 * (t[0] + t[1])).abs() < 1e-6);
        assert!(sine.read(u32::MAX).is_finite());
        assert!((sine.read(u32::MAX) - t[0]).abs() < 1e-3);
    }

    #[test]
    fn level_for_picks_the_fullest_table_that_does_not_alias() {
        let sr = 48_000.0;
        for freq in [10.0, 55.0, 110.0, 440.0, 1_000.0, 4_000.0, 12_000.0] {
            let level = WavetableBank::level_for(freq, sr);
            let allowed = (0.5 * sr / freq).floor() as usize;
            assert!(
                HARMONICS[level] <= allowed || level == MIP_LEVELS - 1,
                "{freq} Hz level {level}"
            );
            if level > 0 {
                assert!(HARMONICS[level - 1] > allowed, "{freq} Hz level {level}");
            }
        }
        assert_eq!(WavetableBank::level_for(10.0, sr), 0);
        assert_eq!(WavetableBank::level_for(20_000.0, sr), MIP_LEVELS - 1);
        assert_eq!(WavetableBank::level_for(440.0, sr), 5);
        // Nonsense in, the fullest table out, no panic.
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(WavetableBank::level_for(bad, sr), 0);
        }
        assert_eq!(WavetableBank::level_for(440.0, f32::NAN), 0);
    }

    #[test]
    fn the_preview_draws_the_wave_and_survives_an_empty_buffer() {
        wave_preview(0.5, &mut []);
        let mut square = [0.0f32; 256];
        wave_preview(1.0, &mut square);
        assert!(square[64] > 0.8 && square[192] < -0.8);
        let mut sine = [0.0f32; 256];
        wave_preview(0.0, &mut sine);
        for (k, y) in sine.iter().enumerate() {
            let want = (TAU * k as f64 / 256.0).sin() as f32;
            assert!((y - want).abs() < 1e-4, "k {k}");
        }
    }
}
