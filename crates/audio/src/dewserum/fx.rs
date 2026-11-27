//! 16-slot modulatable FX rack for DewSerum (Stage 5.3).

use crate::biquad::{Biquad, Response, flush};
use super::FilterKind;
use super::modulation::choice;

pub const FX_SLOTS: usize = 16;
/// Per slot: `[Kind, Bypass(On=1/Off=0), SubMode, P1, P2, P3, Mix]` = 7 params.
pub const FX_SLOT_PARAMS: usize = 7;

pub const FX_KIND: usize = 0;
pub const FX_ON: usize = 1;
pub const FX_SUB: usize = 2;
pub const FX_P1: usize = 3;
pub const FX_P2: usize = 4;
pub const FX_P3: usize = 5;
pub const FX_MIX: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FxKind {
    #[default]
    Empty = 0,
    HyperDim,
    Distortion,
    Filter,
    ChorusFlanger,
    Delay,
    Reverb,
}

pub const FX_KIND_COUNT: usize = 7;

impl FxKind {
    pub const NAMES: [&'static str; FX_KIND_COUNT] = [
        "Empty",
        "Hyper/Dim",
        "Distortion",
        "Filter",
        "Chorus/Flng",
        "Delay",
        "Reverb",
    ];

    pub fn from_param(v: f32) -> Self {
        match choice(v, FX_KIND_COUNT) {
            0 => Self::Empty,
            1 => Self::HyperDim,
            2 => Self::Distortion,
            3 => Self::Filter,
            4 => Self::ChorusFlanger,
            5 => Self::Delay,
            _ => Self::Reverb,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SlotInputs {
    pub kind: FxKind,
    pub active: bool,
    pub sub: f32,
    pub p1: f32,
    pub p2: f32,
    pub p3: f32,
    pub mix: f32,
}

// ---------------------------------------------------------------------------------------------------------------------
// 1. Hyper / Dimension
// ---------------------------------------------------------------------------------------------------------------------

struct HyperDimState {
    buf: [Vec<f32>; 2],
    pos: usize,
    phases: [f32; 4],
}

impl HyperDimState {
    fn new(sample_rate: f32) -> Self {
        let len = ((sample_rate * 0.05) as usize).max(64);
        Self {
            buf: [vec![0.0; len], vec![0.0; len]],
            pos: 0,
            phases: [0.0, 0.25, 0.5, 0.75],
        }
    }

    fn reset(&mut self) {
        self.buf[0usize].fill(0.0);
        self.buf[1usize].fill(0.0);
        self.pos = 0;
    }

    #[allow(clippy::too_many_arguments)]
    fn process(&mut self, l: f32, r: f32, rate: f32, detune: f32, size: f32, mix: f32, sr: f32) -> (f32, f32) {
        let len = self.buf[0usize].len();
        self.buf[0usize][self.pos] = l;
        self.buf[1usize][self.pos] = r;

        let hz = 0.15 + 3.5 * rate * rate;
        let step = hz / sr.max(1.0);
        let depth_samples = (0.002 + 0.016 * detune) * sr;
        let base_delay = (0.004 + 0.022 * size) * sr;

        let mut wet_l = 0.0f32;
        let mut wet_r = 0.0f32;
        for v in 0..4 {
            self.phases[v] = (self.phases[v] + step * (1.0 + 0.07 * v as f32)).fract();
            let lfo = (self.phases[v] * std::f32::consts::TAU).sin();
            let d = (base_delay + lfo * depth_samples * (0.6 + 0.12 * v as f32))
                .clamp(1.0, (len - 2) as f32);
            let di = d as usize;
            let frac = d - di as f32;
            let i0 = (self.pos + len - di) % len;
            let i1 = (i0 + len - 1) % len;
            let ch = v & 1;
            let s = self.buf[ch][i0] * (1.0 - frac) + self.buf[ch][i1] * frac;
            if v % 2 == 0 {
                wet_l += s;
                wet_r -= s * 0.35 * size;
            } else {
                wet_r += s;
                wet_l -= s * 0.35 * size;
            }
        }
        wet_l *= 0.42;
        wet_r *= 0.42;

        self.pos = (self.pos + 1) % len;
        let m = mix.clamp(0.0, 1.0);
        (l + (wet_l - l) * m, r + (wet_r - r) * m)
    }
          }
    // ---------------------------------------------------------------------------------------------------------------------
// 2. Distortion (Tube / Hard / Fold / Crush)
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Default)]
struct DistortionFxState {
    lp: [f32; 2],
    hold: [f32; 2],
    hold_cnt: usize,
}

impl DistortionFxState {
    fn reset(&mut self) {
        self.lp = [0.0; 2];
        self.hold = [0.0; 2];
        self.hold_cnt = 0;
    }

    #[allow(clippy::too_many_arguments)]
    fn process(&mut self, l: f32, r: f32, sub: f32, drive: f32, tone: f32, post: f32, mix: f32) -> (f32, f32) {
        let mode = choice(sub, 4);
        let gain = 1.0 + 28.0 * drive.clamp(0.0, 1.0).powi(2);
        let out_gain = 0.25 + 1.5 * post.clamp(0.0, 1.0);
        let alpha = (0.04 + 0.94 * tone.clamp(0.0, 1.0).powi(2)).clamp(0.02, 0.99);

        let (mut wl, mut wr) = match mode {
            0 => ((l * gain).tanh(), (r * gain).tanh()),
            1 => ((l * gain).clamp(-1.0, 1.0), (r * gain).clamp(-1.0, 1.0)),
            2 => {
                let fold = |x: f32| {
                    let y = (x * (1.0 + 8.0 * drive)).clamp(-8.0, 8.0);
                    (y * std::f32::consts::FRAC_PI_2).sin()
                };
                (fold(l), fold(r))
            }
            _ => {
                let step = 1usize + (drive.clamp(0.0, 1.0).powi(2) * 28.0) as usize;
                let bits = (16.0 - 12.5 * drive.clamp(0.0, 1.0)).max(2.5);
                let levels = 2.0f32.powf(bits);
                if self.hold_cnt == 0 {
                    self.hold[0usize] = (l * levels).round() / levels;
                    self.hold[1usize] = (r * levels).round() / levels;
                    self.hold_cnt = step;
                }
                self.hold_cnt -= 1;
                (self.hold[0usize], self.hold[1usize])
            }
        };

        self.lp[0usize] = flush(self.lp[0usize] + (wl - self.lp[0usize]) * alpha);
        self.lp[1usize] = flush(self.lp[1usize] + (wr - self.lp[1usize]) * alpha);
        wl = self.lp[0usize] * out_gain;
        wr = self.lp[1usize] * out_gain;

        let m = mix.clamp(0.0, 1.0);
        (l + (wl - l) * m, r + (wr - r) * m)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// 3. Filter FX
// ---------------------------------------------------------------------------------------------------------------------

struct FilterFxState {
    f1: [Biquad; 2],
    f2: [Biquad; 2],
    comb: [Vec<f32>; 2],
    comb_pos: usize,
}

impl FilterFxState {
    fn new() -> Self {
        Self {
            f1: [Biquad::default(); 2],
            f2: [Biquad::default(); 2],
            comb: [vec![0.0; 2048], vec![0.0; 2048]],
            comb_pos: 0,
        }
    }

    fn reset(&mut self) {
        for f in &mut self.f1 {
            f.reset();
        }
        for f in &mut self.f2 {
            f.reset();
        }
        self.comb[0usize].fill(0.0);
        self.comb[1usize].fill(0.0);
        self.comb_pos = 0;
    }

    #[allow(clippy::too_many_arguments)]
    fn process(&mut self, l: f32, r: f32, sub: f32, cut: f32, res: f32, drive: f32, mix: f32, sr: f32) -> (f32, f32) {
        let kind = FilterKind::from_param(sub);
        let cutoff = (20.0 * 2.0f32.powf(10.0 * cut.clamp(0.0, 1.0))).clamp(20.0, 0.45 * sr);
        let r_clamped = res.clamp(0.0, 1.0);
        let q = 0.5 * 20.0f32.powf(r_clamped);
        let q24 = 0.5 * 8.0f32.powf(r_clamped);
        let drv = 1.0 + 5.0 * drive.clamp(0.0, 1.0);

        let dl = if drv > 1.001 { (l * drv).tanh() } else { l };
        let dr = if drv > 1.001 { (r * drv).tanh() } else { r };

        let (wl, wr) = match kind {
            FilterKind::Lp12 => {
                self.f1[0usize].set_filter_response(Response::LowPass, cutoff, q, sr);
                self.f1[1usize].set_filter_response(Response::LowPass, cutoff, q, sr);
                (self.f1[0usize].process(dl), self.f1[1usize].process(dr))
            }
            FilterKind::Lp24 => {
                for ch in 0..2 {
                    self.f1[ch].set_filter_response(Response::LowPass, cutoff, q24, sr);
                    self.f2[ch].set_filter_response(Response::LowPass, cutoff, q24, sr);
                }
                (
                    self.f2[0usize].process(self.f1[0usize].process(dl)),
                    self.f2[1usize].process(self.f1[1usize].process(dr)),
                )
            }
            FilterKind::Hp12 => {
                self.f1[0usize].set_filter_response(Response::HighPass, cutoff, q, sr);
                self.f1[1usize].set_filter_response(Response::HighPass, cutoff, q, sr);
                (self.f1[0usize].process(dl), self.f1[1usize].process(dr))
            }
            FilterKind::Hp24 => {
                for ch in 0..2 {
                    self.f1[ch].set_filter_response(Response::HighPass, cutoff, q24, sr);
                    self.f2[ch].set_filter_response(Response::HighPass, cutoff, q24, sr);
                }
                (
                    self.f2[0usize].process(self.f1[0usize].process(dl)),
                    self.f2[1usize].process(self.f1[1usize].process(dr)),
                )
            }
            FilterKind::Bp12 => {
                self.f1[0usize].set_filter_response(Response::BandPass, cutoff, q, sr);
                self.f1[1usize].set_filter_response(Response::BandPass, cutoff, q, sr);
                (self.f1[0usize].process(dl), self.f1[1usize].process(dr))
            }
            FilterKind::Notch => {
                let bw = (1.2 - 0.9 * r_clamped).max(0.15);
                self.f1[0usize].set_parametric_response(Response::Peak, cutoff, bw, -36.0, sr);
                self.f1[1usize].set_parametric_response(Response::Peak, cutoff, bw, -36.0, sr);
                (self.f1[0usize].process(dl), self.f1[1usize].process(dr))
            }
            FilterKind::Comb => {
                let len = self.comb[0usize].len();
                let d = (sr / cutoff).clamp(2.0, (len - 2) as f32);
                let di = d as usize;
                let frac = d - di as f32;
                let fb = 0.25 + 0.70 * r_clamped;
                let i0 = (self.comb_pos + len - di) % len;
                let i1 = (i0 + len - 1) % len;
                let out_l = (dl + (self.comb[0usize][i0] * (1.0 - frac) + self.comb[0usize][i1] * frac) * fb).tanh();
                let out_r = (dr + (self.comb[1usize][i0] * (1.0 - frac) + self.comb[1usize][i1] * frac) * fb).tanh();
                self.comb[0usize][self.comb_pos] = out_l;
                self.comb[1usize][self.comb_pos] = out_r;
                self.comb_pos = (self.comb_pos + 1) % len;
                (out_l, out_r)
            }
        };

        let m = mix.clamp(0.0, 1.0);
        (l + (wl - l) * m, r + (wr - r) * m)
    }
      }
// ---------------------------------------------------------------------------------------------------------------------
// 4. Chorus / Flanger
// ---------------------------------------------------------------------------------------------------------------------

struct ChorusFlangerState {
    buf: [Vec<f32>; 2],
    pos: usize,
    phase: f32,
    fb: [f32; 2],
}

impl ChorusFlangerState {
    fn new(sample_rate: f32) -> Self {
        let len = ((sample_rate * 0.04) as usize).max(64);
        Self {
            buf: [vec![0.0; len], vec![0.0; len]],
            pos: 0,
            phase: 0.0,
            fb: [0.0; 2],
        }
    }

    fn reset(&mut self) {
        self.buf[0usize].fill(0.0);
        self.buf[1usize].fill(0.0);
        self.pos = 0;
        self.phase = 0.0;
        self.fb = [0.0; 2];
    }

    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        l: f32,
        r: f32,
        sub: f32,
        rate: f32,
        depth: f32,
        feedback: f32,
        mix: f32,
        sr: f32,
    ) -> (f32, f32) {
        let is_flanger = choice(sub, 2) == 1;
        let len = self.buf[0usize].len();
        let hz = 0.08 + 6.0 * rate.clamp(0.0, 1.0).powi(2);
        self.phase = (self.phase + hz / sr.max(1.0)).fract();

        let (base_ms, span_ms, fb_amt) = if is_flanger {
            (0.4, 3.2 * depth.clamp(0.0, 1.0), 0.85 * feedback.clamp(0.0, 1.0))
        } else {
            (7.0, 9.0 * depth.clamp(0.0, 1.0), 0.35 * feedback.clamp(0.0, 1.0))
        };

        let lfo_l = 0.5 + 0.5 * (self.phase * std::f32::consts::TAU).sin();
        let lfo_r = 0.5 + 0.5 * ((self.phase + 0.25).fract() * std::f32::consts::TAU).sin();

        let mut outs = [0.0f32; 2];
        let inputs = [l, r];
        let lfos = [lfo_l, lfo_r];
        for ch in 0..2 {
            self.buf[ch][self.pos] = flush(inputs[ch] + self.fb[ch] * fb_amt);
            let delay = ((base_ms + lfos[ch] * span_ms) * 0.001 * sr).clamp(1.0, (len - 2) as f32);
            let di = delay as usize;
            let frac = delay - di as f32;
            let i0 = (self.pos + len - di) % len;
            let i1 = (i0 + len - 1) % len;
            let wet = self.buf[ch][i0] * (1.0 - frac) + self.buf[ch][i1] * frac;
            self.fb[ch] = wet;
            outs[ch] = wet;
        }
        self.pos = (self.pos + 1) % len;

        let m = mix.clamp(0.0, 1.0);
        (l + (outs[0usize] - l) * m, r + (outs[1usize] - r) * m)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// 5. Delay (Stereo / Ping-Pong, BPM synced)
// ---------------------------------------------------------------------------------------------------------------------

struct DelayFxState {
    buf: [Vec<f32>; 2],
    pos: usize,
    lp: [f32; 2],
}

impl DelayFxState {
    fn new(sample_rate: f32) -> Self {
        let len = ((sample_rate * 2.5) as usize).max(256);
        Self {
            buf: [vec![0.0; len], vec![0.0; len]],
            pos: 0,
            lp: [0.0; 2],
        }
    }

    fn reset(&mut self) {
        self.buf[0usize].fill(0.0);
        self.buf[1usize].fill(0.0);
        self.pos = 0;
        self.lp = [0.0; 2];
    }

    #[allow(clippy::too_many_arguments)]
    fn process(
        &mut self,
        l: f32,
        r: f32,
        sub: f32,
        time: f32,
        feedback: f32,
        tone: f32,
        mix: f32,
        tempo_bpm: f32,
        sr: f32,
    ) -> (f32, f32) {
        const BEATS: [f32; 8] = [0.125, 0.25, 0.375, 0.5, 0.75, 1.0, 1.5, 2.0];
        let ping_pong = choice(sub, 2) == 1;
        let beats = BEATS[choice(time, BEATS.len())];
        let bpm = if tempo_bpm.is_finite() && tempo_bpm > 0.0 { tempo_bpm } else { 120.0 };
        let len = self.buf[0usize].len();
        let d_samples = ((60.0 / bpm) * beats * sr).round().clamp(1.0, (len - 2) as f32) as usize;

        let idx_l = (self.pos + len - d_samples) % len;
        let idx_r = if ping_pong {
            (self.pos + len - (d_samples / 2).max(1)) % len
        } else {
            idx_l
        };

        let raw_l = self.buf[0usize][idx_l];
        let raw_r = self.buf[1usize][idx_r];

        let alpha = (0.04 + 0.94 * tone.clamp(0.0, 1.0).powi(2)).clamp(0.02, 0.99);
        self.lp[0usize] = flush(self.lp[0usize] + (raw_l - self.lp[0usize]) * alpha);
        self.lp[1usize] = flush(self.lp[1usize] + (raw_r - self.lp[1usize]) * alpha);

        let fb = 0.94 * feedback.clamp(0.0, 1.0);
        if ping_pong {
            let mono = 0.5 * (l + r);
            self.buf[0usize][self.pos] = flush(mono + self.lp[1usize] * fb);
            self.buf[1usize][self.pos] = flush(self.lp[0usize] * fb);
        } else {
            self.buf[0usize][self.pos] = flush(l + self.lp[0usize] * fb);
            self.buf[1usize][self.pos] = flush(r + self.lp[1usize] * fb);
        }
        self.pos = (self.pos + 1) % len;

        let m = mix.clamp(0.0, 1.0);
        (l + self.lp[0usize] * m, r + self.lp[1usize] * m)
    }
}
// ---------------------------------------------------------------------------------------------------------------------
// 6. Reverb (Schroeder stereo FDN/comb+allpass)
// ---------------------------------------------------------------------------------------------------------------------

struct ReverbFxState {
    combs_l: [Vec<f32>; 4],
    combs_r: [Vec<f32>; 4],
    comb_idx: [usize; 4],
    damp_l: [f32; 4],
    damp_r: [f32; 4],
    ap_l: [Vec<f32>; 2],
    ap_r: [Vec<f32>; 2],
    ap_idx: [usize; 2],
}

impl ReverbFxState {
    fn new(sample_rate: f32) -> Self {
        let scale = (sample_rate / 48_000.0).max(0.5);
        let lens = [1687usize, 1801, 2053, 2251];
        let ap_lens = [556usize, 441];
        Self {
            combs_l: std::array::from_fn(|i| vec![0.0; ((lens[i] as f32 * scale) as usize).max(32)]),
            combs_r: std::array::from_fn(|i| vec![0.0; (((lens[i] + 23) as f32 * scale) as usize).max(32)]),
            comb_idx: [0; 4],
            damp_l: [0.0; 4],
            damp_r: [0.0; 4],
            ap_l: std::array::from_fn(|i| vec![0.0; ((ap_lens[i] as f32 * scale) as usize).max(16)]),
            ap_r: std::array::from_fn(|i| vec![0.0; (((ap_lens[i] + 17) as f32 * scale) as usize).max(16)]),
            ap_idx: [0; 2],
        }
    }

    fn reset(&mut self) {
        for c in &mut self.combs_l {
            c.fill(0.0);
        }
        for c in &mut self.combs_r {
            c.fill(0.0);
        }
        self.comb_idx = [0; 4];
        self.damp_l = [0.0; 4];
        self.damp_r = [0.0; 4];
        for a in &mut self.ap_l {
            a.fill(0.0);
        }
        for a in &mut self.ap_r {
            a.fill(0.0);
        }
        self.ap_idx = [0; 2];
    }

    fn process(&mut self, l: f32, r: f32, size: f32, decay: f32, damp: f32, mix: f32) -> (f32, f32) {
        let fb = (0.68 + 0.29 * decay.clamp(0.0, 1.0)).min(0.97);
        let d_coef = 0.15 + 0.72 * damp.clamp(0.0, 1.0);
        let input = 0.5 * (l + r) * (0.35 + 0.45 * size.clamp(0.0, 1.0));

        let mut acc_l = 0.0f32;
        let mut acc_r = 0.0f32;
        for i in 0..4 {
            let len_l = self.combs_l[i].len();
            let len_r = self.combs_r[i].len();
            let il = self.comb_idx[i] % len_l;
            let ir = self.comb_idx[i] % len_r;

            let out_l = self.combs_l[i][il];
            let out_r = self.combs_r[i][ir];

            self.damp_l[i] = flush(out_l * (1.0 - d_coef) + self.damp_l[i] * d_coef);
            self.damp_r[i] = flush(out_r * (1.0 - d_coef) + self.damp_r[i] * d_coef);

            self.combs_l[i][il] = flush(input + self.damp_l[i] * fb);
            self.combs_r[i][ir] = flush(input + self.damp_r[i] * fb);

            acc_l += out_l;
            acc_r += out_r;
            self.comb_idx[i] = self.comb_idx[i].wrapping_add(1);
        }

        let mut wl = acc_l * 0.25;
        let mut wr = acc_r * 0.25;
        for a in 0..2 {
            let len_l = self.ap_l[a].len();
            let len_r = self.ap_r[a].len();
            let il = self.ap_idx[a] % len_l;
            let ir = self.ap_idx[a] % len_r;

            let buf_l = self.ap_l[a][il];
            let buf_r = self.ap_r[a][ir];

            let next_l = flush(wl + buf_l * 0.5);
            let next_r = flush(wr + buf_r * 0.5);
            wl = buf_l - wl * 0.5;
            wr = buf_r - wr * 0.5;

            self.ap_l[a][il] = next_l;
            self.ap_r[a][ir] = next_r;
            self.ap_idx[a] = self.ap_idx[a].wrapping_add(1);
        }

        let m = mix.clamp(0.0, 1.0);
        (l + wl * m, r + wr * m)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// 16-slot FX Rack
// ---------------------------------------------------------------------------------------------------------------------

struct FxSlotState {
    hyper: HyperDimState,
    dist: DistortionFxState,
    filter: FilterFxState,
    chorus: ChorusFlangerState,
    delay: DelayFxState,
    reverb: ReverbFxState,
}

impl FxSlotState {
    fn new(sample_rate: f32) -> Self {
        Self {
            hyper: HyperDimState::new(sample_rate),
            dist: DistortionFxState::default(),
            filter: FilterFxState::new(),
            chorus: ChorusFlangerState::new(sample_rate),
            delay: DelayFxState::new(sample_rate),
            reverb: ReverbFxState::new(sample_rate),
        }
    }

    fn reset(&mut self) {
        self.hyper.reset();
        self.dist.reset();
        self.filter.reset();
        self.chorus.reset();
        self.delay.reset();
        self.reverb.reset();
    }

    #[inline]
    fn process(&mut self, l: f32, r: f32, in_s: &SlotInputs, tempo_bpm: f32, sr: f32) -> (f32, f32) {
        if !in_s.active || matches!(in_s.kind, FxKind::Empty) || in_s.mix <= 1e-5 {
            return (l, r);
        }
        match in_s.kind {
            FxKind::Empty => (l, r),
            FxKind::HyperDim => self.hyper.process(l, r, in_s.p1, in_s.p2, in_s.p3, in_s.mix, sr),
            FxKind::Distortion => self.dist.process(l, r, in_s.sub, in_s.p1, in_s.p2, in_s.p3, in_s.mix),
            FxKind::Filter => self.filter.process(l, r, in_s.sub, in_s.p1, in_s.p2, in_s.p3, in_s.mix, sr),
            FxKind::ChorusFlanger => {
                self.chorus.process(l, r, in_s.sub, in_s.p1, in_s.p2, in_s.p3, in_s.mix, sr)
            }
            FxKind::Delay => {
                self.delay.process(l, r, in_s.sub, in_s.p1, in_s.p2, in_s.p3, in_s.mix, tempo_bpm, sr)
            }
            FxKind::Reverb => self.reverb.process(l, r, in_s.p1, in_s.p2, in_s.p3, in_s.mix),
        }
    }
}

pub struct FxRack {
    slots: [FxSlotState; FX_SLOTS],
    sample_rate: f32,
}

impl FxRack {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            slots: std::array::from_fn(|_| FxSlotState::new(sample_rate)),
            sample_rate,
        }
    }

    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.reset();
        }
    }

    #[inline]
    pub fn process_frame(&mut self, mut l: f32, mut r: f32, inputs: &[SlotInputs; FX_SLOTS], tempo_bpm: f32) -> (f32, f32) {
        let sr = self.sample_rate;
        for (slot, inp) in self.slots.iter_mut().zip(inputs.iter()) {
            (l, r) = slot.process(l, r, inp, tempo_bpm, sr);
        }
        (l, r)
    }
               }
                                    
          
