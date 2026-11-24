//! DewSerum's multi-wavetable bank: `Basic Shapes` plus 14 embedded Serum-format `.wav` tables
//! (2048 samples per frame), band-limited into [`MIP_LEVELS`] anti-aliased copies on first load.

use std::f64::consts::{PI, TAU};
use std::sync::OnceLock;

pub const FRAME_LEN: usize = 2048;
pub const BASIC_FRAMES: usize = 4;
pub const MAX_WAV_FRAMES: usize = 64;
pub const TABLE_COUNT: usize = 15;
pub const TABLE_NAMES: [&str; TABLE_COUNT] = [
    "Basic Shapes",
    "Analog PWM 02",
    "Analog PWM 05",
    "Analog Square 05",
    "FM 01",
    "FM 05",
    "Growl 01",
    "Growl 02",
    "Growl 03",
    "Growl 04",
    "Growl 12",
    "Hyper 04",
    "Hyper 05",
    "Hyper 06",
    "Hyper 07",
];

const EMBEDDED_WAVS: [&[u8]; TABLE_COUNT - 1] = [
    include_bytes!("wtables/Analog PWM Custom 02.wav"),
    include_bytes!("wtables/Analog PWM Custom 05.wav"),
    include_bytes!("wtables/Analog PWM Square 05.wav"),
    include_bytes!("wtables/FM 01.wav"),
    include_bytes!("wtables/FM 05.wav"),
    include_bytes!("wtables/Growl 01.wav"),
    include_bytes!("wtables/Growl 02.wav"),
    include_bytes!("wtables/Growl 03.wav"),
    include_bytes!("wtables/Growl 04.wav"),
    include_bytes!("wtables/Growl 12.wav"),
    include_bytes!("wtables/Hyper 04.wav"),
    include_bytes!("wtables/Hyper 05.wav"),
    include_bytes!("wtables/Hyper 06.wav"),
    include_bytes!("wtables/Hyper 07.wav"),
];

pub const MIP_LEVELS: usize = 10;
pub const HARMONICS: [usize; MIP_LEVELS] = [1023usize, 511, 255, 127, 63, 31, 15, 7, 3, 1];
pub const PREVIEW_LEVEL: usize = 3;

const STRIDE: usize = FRAME_LEN + 1;
const INDEX_SHIFT: u32 = 32 - FRAME_LEN.trailing_zeros();
const FRAC_MASK: u32 = (1 << INDEX_SHIFT) - 1;
const FRAC_SCALE: f32 = 1.0 / (1u32 << INDEX_SHIFT) as f32;
const PHASE_SCALE: f64 = 4_294_967_296.0;

fn basic_amplitude(frame: usize, h: usize) -> f64 {
    let odd = h % 2 == 1;
    match frame {
        0 => {
            if h == 1 {
                1.0
            } else {
                0.0
            }
        }
        1 if odd => {
            let sign = if (h / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
            8.0 / (PI * PI) * sign / (h * h) as f64
        }
        2 => 2.0 / PI / h as f64,
        3 if odd => 4.0 / PI / h as f64,
        _ => 0.0,
    }
}
fn parse_wav_samples(bytes: &[u8]) -> Vec<f32> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Vec::new();
    }
    let mut pos = 12usize;
    let mut format_tag = 0u16;
    let mut channels = 1u16;
    let mut bits_per_sample = 16u16;
    let mut raw_data: &[u8] = &[];

    while pos + 8 <= bytes.len() {
        let chunk_id = &bytes[pos..pos + 4];
        let chunk_len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let data_start = pos + 8;
        let data_end = data_start.saturating_add(chunk_len).min(bytes.len());

        if chunk_id == b"fmt " && data_end - data_start >= 16 {
            format_tag = u16::from_le_bytes([bytes[data_start], bytes[data_start + 1]]);
            channels = u16::from_le_bytes([bytes[data_start + 2], bytes[data_start + 3]]).max(1);
            bits_per_sample = u16::from_le_bytes([bytes[data_start + 14], bytes[data_start + 15]]);
        } else if chunk_id == b"data" {
            raw_data = &bytes[data_start..data_end];
        }

        pos = data_start.saturating_add(chunk_len + (chunk_len & 1));
    }

    if raw_data.is_empty() {
        return Vec::new();
    }

    let ch = usize::from(channels);
    match (format_tag, bits_per_sample) {
        (1, 16) => {
            let step = 2 * ch;
            raw_data
                .chunks_exact(step)
                .map(|frame| i16::from_le_bytes([frame[0], frame[1]]) as f32 / 32768.0)
                .collect()
        }
        (1, 24) => {
            let step = 3 * ch;
            raw_data
                .chunks_exact(step)
                .map(|frame| {
                    let sign = if frame[2] & 0x80 != 0 { 0xFF } else { 0x00 };
                    let val = i32::from_le_bytes([frame[0], frame[1], frame[2], sign]);
                    val as f32 / 8_388_608.0
                })
                .collect()
        }
        (3, 32) => {
            let step = 4 * ch;
            raw_data
                .chunks_exact(step)
                .map(|frame| {
                    let v = f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
                    if v.is_finite() { v } else { 0.0 }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn radix2_fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2usize;
    while len <= n {
        let half = len >> 1;
        let ang = sign * TAU / len as f64;
        let (w_sin, w_cos) = ang.sin_cos();
        for start in (0..n).step_by(len) {
            let (mut wr, mut wi) = (1.0f64, 0.0f64);
            for k in 0..half {
                let u_re = re[start + k];
                let u_im = im[start + k];
                let v_re = re[start + k + half] * wr - im[start + k + half] * wi;
                let v_im = re[start + k + half] * wi + im[start + k + half] * wr;
                re[start + k] = u_re + v_re;
                im[start + k] = u_im + v_im;
                re[start + k + half] = u_re - v_re;
                im[start + k + half] = u_im - v_im;
                let next_wr = wr * w_cos - wi * w_sin;
                wi = wr * w_sin + wi * w_cos;
                wr = next_wr;
            }
        }
        len <<= 1;
    }

    if inverse {
        let inv_n = 1.0 / n as f64;
        for i in 0..n {
            re[i] *= inv_n;
            im[i] *= inv_n;
        }
    }
}

fn build_frame_mips(samples: &[f32], out: &mut [f32]) {
    let mut base_re = vec![0.0f64; FRAME_LEN];
    let mut base_im = vec![0.0f64; FRAME_LEN];
    for i in 0..FRAME_LEN {
        base_re[i] = f64::from(samples[i]);
    }
    radix2_fft(&mut base_re, &mut base_im, false);
    // Remove DC offset so wavetables never introduce clicks
    base_re[0] = 0.0;
    base_im[0] = 0.0;

    let mut re = vec![0.0f64; FRAME_LEN];
    let mut im = vec![0.0f64; FRAME_LEN];
    for level in 0..MIP_LEVELS {
        let max_h = HARMONICS[level];
        re.fill(0.0);
        im.fill(0.0);
        for h in 1..=max_h {
            re[h] = base_re[h];
            im[h] = base_im[h];
            re[FRAME_LEN - h] = base_re[FRAME_LEN - h];
            im[FRAME_LEN - h] = base_im[FRAME_LEN - h];
        }
        radix2_fft(&mut re, &mut im, true);
        let offset = level * STRIDE;
        let table = &mut out[offset..offset + STRIDE];
        for n in 0..FRAME_LEN {
            table[n] = re[n] as f32;
        }
        table[FRAME_LEN] = table[0];
    }

    let peak = out[0..FRAME_LEN]
        .iter()
        .fold(0.0f32, |p, &s| p.max(s.abs()));
    let scale = if peak > 1e-6 { 1.0 / peak } else { 1.0 };
    for s in out.iter_mut() {
        *s *= scale;
    }
                                                     }
struct TableMeta {
    start_frame: usize,
    frame_count: usize,
}

pub struct WavetableBank {
    data: Box<[f32]>,
    tables: [TableMeta; TABLE_COUNT],
}

impl WavetableBank {
    fn build() -> Self {
        let mut data = Vec::new();
        let mut tables = std::array::from_fn(|_| TableMeta {
            start_frame: 0,
            frame_count: BASIC_FRAMES,
        });

        // Table 0: Basic Shapes (Sine, Triangle, Saw, Square)
        let sine: Vec<f64> = (0..FRAME_LEN)
            .map(|n| (TAU * n as f64 / FRAME_LEN as f64).sin())
            .collect();
        let mut basic_data = vec![0.0f32; BASIC_FRAMES * MIP_LEVELS * STRIDE];
        let mut acc = vec![0.0f64; FRAME_LEN];
        for frame in 0..BASIC_FRAMES {
            acc.fill(0.0);
            let mut added = 0;
            for level in (0..MIP_LEVELS).rev() {
                while added < HARMONICS[level] {
                    added += 1;
                    let a = basic_amplitude(frame, added);
                    if a != 0.0 {
                        for (n, sample) in acc.iter_mut().enumerate() {
                            *sample += a * sine[added * n % FRAME_LEN];
                        }
                    }
                }
                let offset = (frame * MIP_LEVELS + level) * STRIDE;
                let table = &mut basic_data[offset..offset + STRIDE];
                for (out, &sample) in table.iter_mut().zip(&acc) {
                    *out = sample as f32;
                }
                table[FRAME_LEN] = table[0usize];
            }
            let full = (frame * MIP_LEVELS) * STRIDE;
            let peak = basic_data[full..full + FRAME_LEN]
                .iter()
                .fold(0.0f32, |peak, s| peak.max(s.abs()));
            let scale = if peak > 0.0 { 1.0 / peak } else { 1.0 };
            let start = frame * MIP_LEVELS * STRIDE;
            basic_data[start..start + MIP_LEVELS * STRIDE]
                .iter_mut()
                .for_each(|s| *s *= scale);
        }
        data.extend_from_slice(&basic_data);
        let mut total_frames = BASIC_FRAMES;

        // Tables 1..15: Embedded Serum-format .wav files
        for (idx, &wav_bytes) in EMBEDDED_WAVS.iter().enumerate() {
            let table_idx = idx + 1;
            let samples = parse_wav_samples(wav_bytes);
            let raw_frames = samples.len() / FRAME_LEN;
            if raw_frames == 0 {
                tables[table_idx] = TableMeta {
                    start_frame: 0,
                    frame_count: BASIC_FRAMES,
                };
                continue;
            }
            let count = raw_frames.min(MAX_WAV_FRAMES);
            let start_frame = total_frames;
            let mut tbl_buf = vec![0.0f32; count * MIP_LEVELS * STRIDE];
            for f in 0..count {
                let src_idx = if count == 1 {
                    0
                } else {
                    f * (raw_frames - 1) / (count - 1)
                };
                let src = &samples[src_idx * FRAME_LEN..(src_idx + 1) * FRAME_LEN];
                let dst_off = f * MIP_LEVELS * STRIDE;
                build_frame_mips(src, &mut tbl_buf[dst_off..dst_off + MIP_LEVELS * STRIDE]);
            }
            data.extend_from_slice(&tbl_buf);
            tables[table_idx] = TableMeta {
                start_frame,
                frame_count: count,
            };
            total_frames += count;
        }

        Self {
            data: data.into_boxed_slice(),
            tables,
        }
    }

    pub fn table_slice(&self, global_frame: usize, level: usize) -> &[f32] {
        let max_frame = self.data.len() / (MIP_LEVELS * STRIDE);
        let f = global_frame.min(max_frame.saturating_sub(1));
        let offset = (f * MIP_LEVELS + level.min(MIP_LEVELS - 1)) * STRIDE;
        &self.data[offset..offset + STRIDE]
    }

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

    pub fn morph_for(&self, table_idx: usize, pos: f32) -> Morph {
        let meta = &self.tables[table_idx.min(TABLE_COUNT - 1)];
        let pos = if pos.is_finite() {
            pos.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if meta.frame_count <= 1 {
            return Morph {
                lo: meta.start_frame,
                hi: meta.start_frame,
                mix: 0.0,
            };
        }
        let x = pos * (meta.frame_count - 1) as f32;
        let local_lo = (x as usize).min(meta.frame_count - 2);
        let lo = meta.start_frame + local_lo;
        Morph {
            lo,
            hi: lo + 1,
            mix: x - local_lo as f32,
        }
    }

    pub fn reader(&self, table_idx: usize, level: usize, pos: f32) -> Reader<'_> {
        let morph = self.morph_for(table_idx, pos);
        Reader {
            a: self.table_slice(morph.lo, level),
            b: self.table_slice(morph.hi, level),
            mix: morph.mix,
        }
    }
}

pub fn bank() -> &'static WavetableBank {
    static BANK: OnceLock<WavetableBank> = OnceLock::new();
    BANK.get_or_init(WavetableBank::build)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Morph {
    pub lo: usize,
    pub hi: usize,
    pub mix: f32,
}

#[derive(Clone, Copy)]
pub struct Reader<'a> {
    a: &'a [f32],
    b: &'a [f32],
    mix: f32,
}

impl Reader<'_> {
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

pub fn wave_preview(table_idx: usize, pos: f32, out: &mut [f32]) {
    let reader = bank().reader(table_idx, PREVIEW_LEVEL, pos);
    let n = out.len();
    for (k, y) in out.iter_mut().enumerate() {
        *y = reader.read((k as f64 / n as f64 * PHASE_SCALE) as u32);
    }
}
