//! DewSerum panel (type 53), card 16.5 U.

use std::f32::consts::PI;
use std::path::Path;

use eframe::egui::{Align2, Color32, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use honeydew_audio::dewserum::modulation::{
    CURVE_PARAMS, CurvePreset, LfoCurve, MAX_POINTS, choice,
};
use honeydew_audio::dewserum::wavetable::{CUSTOM_MENU_INDEX, TABLE_MENU_COUNT, wave_preview};
use honeydew_audio::dewserum::{ANALYSIS_LEN, ENV_COUNT, FilterKind, LFO_COUNT, SCOPE_SAMPLES, param};

use crate::flm::chunk::{ChunkIter, ChunkWriter, ContentPath};
use crate::metrics::FontSize;
use crate::panels::build::*;
use crate::panels::live_analysis;
use crate::ui::UiCtx;

pub const TYPE_ID: u16 = 53;

const TABS: &str = "OSC A · OSC B · SUB/NOISE · FILTER · ENV · LFO · MATRIX · MASTER";
const ENV_TABS: &str = "ENV 1 · ENV 2 · ENV 3 · ENV 4";
const LFO_TABS: &str = "LFO 1 · LFO 2 · LFO 3 · LFO 4";
const MATRIX_TABS: &str = "Slots 1–4 · Slots 5–8";

const ENV_TAB: &str = "ds_env_tab";
const LFO_TAB: &str = "ds_lfo_tab";
const MAT_TAB: &str = "ds_mat_tab";

const T_CUTOFF: usize = 128;
const T_RES: usize = 129;
const T_WT_A: usize = 130;
const T_WT_B: usize = 131;
const T_SYNC_A: usize = 134;
const T_SYNC_B: usize = 135;
const T_FM: usize = 136;
const T_ACTIVE: usize = 141;

const NOT_OSC_TABS: [Show; 2] = [Show::Not(&Show::Tab(0)), Show::Not(&Show::Tab(1))];

pub fn wire(module: &mut ModuleInstance, state: &mut PanelState) {
    for osc in 0..2 {
        let name = crate::audio_engine::samples::dewserum_table_path(module, osc).and_then(|p| {
            let (ContentPath::Shorthand { relative: text, .. } | ContentPath::Plain(text)) = p;
            Some(Path::new(&text).file_stem()?.to_string_lossy().into_owned())
        });
        state.texts.insert(
            if osc == 0 { 10051 } else { 20051 },
            name.unwrap_or_default(),
        );
    }
    let has_custom = choice(module.params[param::OSC_A_TABLE], TABLE_MENU_COUNT) == CUSTOM_MENU_INDEX
        || choice(module.params[param::OSC_B_TABLE], TABLE_MENU_COUNT) == CUSTOM_MENU_INDEX
        || module.file.custom_data.is_some();
    if has_custom && state.tab <= 1 {
        let osc_idx = state.tab as u8;
        let previous = module.file.custom_data.as_deref().unwrap_or_default();
        let current_tag = ChunkIter::new(previous, "CSTM")
            .map_while(Result::ok)
            .find(|c| c.id == *b"WTOS")
            .and_then(|c| c.payload.first().copied());
        if current_tag != Some(osc_idx) {
            let kept: Vec<_> = ChunkIter::new(previous, "CSTM")
                .map_while(Result::ok)
                .filter(|c| c.id != *b"WTOS" && c.payload.len() % 4 == 0)
                .collect();
            let mut writer = ChunkWriter::default();
            let _ = writer.chunk(*b"WTOS", |w| w.raw(&[osc_idx, 0, 0, 0]));
            for c in kept {
                let _ = writer.chunk(c.id, |w| w.raw(c.payload));
            }
            module.file.custom_data = Some(writer.into_bytes());
        }
    }
}

pub fn custom_picked(module: &ModuleInstance, changed: &[i32]) -> bool {
    if module.type_id != TYPE_ID {
        return false;
    }
    (changed.contains(&(param::OSC_A_TABLE as i32))
        && choice(module.params[param::OSC_A_TABLE], TABLE_MENU_COUNT) == CUSTOM_MENU_INDEX)
        || (changed.contains(&(param::OSC_B_TABLE as i32))
            && choice(module.params[param::OSC_B_TABLE], TABLE_MENU_COUNT) == CUSTOM_MENU_INDEX)
}

pub fn panel() -> Option<Panel> {
    Some(Panel::new(vstack([
        tabs(100, TABS).h(Size::U(1.25)),
        hstack([
            vstack([
                spinner(10050, "OSC A Table")
                    .p(param::OSC_A_TABLE)
                    .items_per_column(5)
                    .show(Show::Tab(0)),
                spinner(20050, "OSC B Table")
                    .p(param::OSC_B_TABLE)
                    .items_per_column(5)
                    .show(Show::Tab(1)),
                text(10051, "")
                    .font(FontSize::Pt10)
                    .align(Align2::LEFT_CENTER)
                    .ink(Ink::Channel)
                    .show(Show::Tab(0)),
                text(20051, "")
                    .font(FontSize::Pt10)
                    .align(Align2::LEFT_CENTER)
                    .ink(Ink::Channel)
                    .show(Show::Tab(1)),
                text(30051, "Output Scope")
                    .font(FontSize::Pt10)
                    .align(Align2::LEFT_CENTER)
                    .ink(Ink::Text)
                    .show(Show::All(&NOT_OSC_TABS)),
            ])
            .w(Size::Flex(1.25)),
            display(900)
                .margins(0.15, 0.15, 0.15, 0.15)
                .w(Size::Flex(1.0))
                .draw(draw_output_scope),
        ])
        .h(Size::U(1.95)),
        osc_page(
            10000,
            param::OSC_BASE[0usize],
            param::OSC_A_SYNC,
            param::OSC_A_WARP_MODE,
            param::OSC_A_WARP,
            Some(param::OSC_A_FM),
            draw_wave_a,
        )
        .show(Show::Tab(0)),
        osc_page(
            20000,
            param::OSC_BASE[1usize],
            param::OSC_B_SYNC,
            param::OSC_B_WARP_MODE,
            param::OSC_B_WARP,
            None,
            draw_wave_b,
        )
        .show(Show::Tab(1)),
        sub_noise_page().show(Show::Tab(2)),
        filter_page().show(Show::Tab(3)),
        env_page().show(Show::Tab(4)),
        lfo_page().show(Show::Tab(5)),
        matrix_page().show(Show::Tab(6)),
        master_page().show(Show::Tab(7)),
    ])))
}

fn osc_page(
    id_base: u32,
    base: usize,
    sync_param: usize,
    warp_mode_param: usize,
    warp_param: usize,
    fm_param: Option<usize>,
    draw_fn: Draw,
) -> Part {
    let mut bottom = vec![
        knob(id_base + 400, "Pitch").p(base + param::PITCH),
        knob(id_base + 500, "Fine").p(base + param::FINE),
        spinner(id_base + 600, "Unison").p(base + param::UNISON),
        knob(id_base + 650, "Sync").p(sync_param),
        spinner(id_base + 660, "Warp Mode").p(warp_mode_param),
        knob(id_base + 670, "Warp").p(warp_param),
    ];
    if let Some(fm) = fm_param {
        bottom.push(knob(id_base + 680, "FM <- B").p(fm));
    }
    bottom.extend([
        v_slider(id_base + 700, "Detune").p(base + param::DETUNE),
        v_slider(id_base + 800, "Blend").p(base + param::BLEND),
        v_slider(id_base + 900, "Width").p(base + param::WIDTH),
    ]);

    vstack([
        hstack([
            display(id_base + 100)
                .margins(0.2, 0.2, 0.2, 0.2)
                .w(Size::Flex(1.6))
                .draw(draw_fn),
            knob(id_base + 200, "WT Pos").p(base + param::WT_POS),
            knob(id_base + 300, "Level").p(base + param::LEVEL),
        ])
        .h(Size::U(4.6)),
        hstack(bottom),
    ])
}

fn sub_noise_page() -> Part {
    vstack([
        hstack([
            spinner(25100, "Sub Shape").p(param::SUB_SHAPE),
            spinner(25200, "Sub Oct").p(param::SUB_OCTAVE),
            toggle(25300, "Direct Out").p(param::SUB_DIRECT),
            knob(25400, "Sub Level").p(param::SUB_LEVEL),
        ]),
        hstack([
            spinner(26100, "Noise Type").p(param::NOISE_KIND),
            knob(26200, "Color").p(param::NOISE_COLOR),
            knob(26300, "Noise Level").p(param::NOISE_LEVEL),
        ]),
    ])
}

fn filter_page() -> Part {
    vstack([
        list(30100, "LP 12 · LP 24 · HP 12 · HP 24 · BP 12 · Notch · Comb", Bar::Bottom)
            .p(param::FILTER_TYPE)
            .h(Size::U(1.15)),
        display(30200)
            .margins(0.2, 0.2, 0.2, 0.2)
            .h(Size::U(4.5))
            .draw(draw_filter_curve),
        hstack([
            knob(30300, "Cutoff").p(param::CUTOFF),
            knob(30400, "Resonance").p(param::RESONANCE),
            knob(30500, "Drive").p(param::FILTER_DRIVE),
            knob(30600, "Key Track").p(param::KEY_TRACK),
        ]),
    ])
}

fn env_page() -> Part {
    let sub_pages: Vec<Part> = (0..ENV_COUNT)
        .map(|e| {
            let at = param::env_at(e);
            let b = 41000 + (e as u32) * 100;
            vstack([
                display(b + 50)
                    .margins(0.2, 0.2, 0.2, 0.2)
                    .h(Size::U(4.5))
                    .draw(match e {
                        0 => draw_env_0,
                        1 => draw_env_1,
                        2 => draw_env_2,
                        _ => draw_env_3,
                    }),
                hstack([
                    knob(b + 1, "Attack").p(at),
                    knob(b + 2, "Decay").p(at + 1),
                    knob(b + 3, "Sustain").p(at + 2),
                    knob(b + 4, "Release").p(at + 3),
                ]),
            ])
            .show(Show::Setting(40100, e))
        })
        .collect();

    let mut children = vec![list(40100, ENV_TABS, Bar::Bottom).h(Size::U(1.15))];
    children.extend(sub_pages);
    vstack(children)
}

fn lfo_page() -> Part {
    let sub_pages: Vec<Part> = (0..LFO_COUNT)
        .map(|l| {
            let at = param::lfo_at(l);
            let b = 51000 + (l as u32) * 100;
            let draw_fn = match l {
                0 => draw_lfo_0,
                1 => draw_lfo_1,
                2 => draw_lfo_2,
                _ => draw_lfo_3,
            };
            vstack([
                display(b + 50)
                    .margins(0.2, 0.2, 0.2, 0.2)
                    .h(Size::U(4.5))
                    .draw(draw_fn),
                hstack([
                    knob(b + 1, "Rate").p(at + param::LFO_RATE),
                    spinner(b + 2, "Mode").p(at + param::LFO_MODE),
                    spinner(b + 3, "Beat").p(at + param::LFO_BEAT),
                    spinner(b + 4, "Trig").p(at + param::LFO_TRIG),
                ]),
            ])
            .show(Show::Setting(50100, l))
        })
        .collect();

    let mut children = vec![list(50100, LFO_TABS, Bar::Bottom).h(Size::U(1.15))];
    children.extend(sub_pages);
    vstack(children)
}

fn matrix_page() -> Part {
    let slot_row = |slot: usize| {
        let at = param::MATRIX + slot * 3;
        let b = 61000 + (slot as u32) * 10;
        hstack([
            spinner(b + 1, "Source").p(at),
            spinner(b + 2, "Dest").p(at + 1).items_per_column(7),
            h_slider(b + 3, "Depth").p(at + 2),
        ])
    };

    vstack([
        list(60100, MATRIX_TABS, Bar::Bottom).h(Size::U(1.15)),
        vstack([slot_row(0), slot_row(1), slot_row(2), slot_row(3)])
            .show(Show::Setting(60100, 0)),
        vstack([slot_row(4), slot_row(5), slot_row(6), slot_row(7)])
            .show(Show::Setting(60100, 1)),
    ])
}

fn master_page() -> Part {
    hstack([
        spinner(70100, "Mode").p(param::VOICE_MODE),
        knob(70150, "Glide").p(param::GLIDE_TIME),
        spinner(70200, "Voices").p(param::VOICES),
        knob(70300, "Master").p(param::MASTER),
    ])
}

fn draw_output_scope(
    ui: &mut Ui,
    cx: &UiCtx,
    _id: Id,
    r: Rect,
    module: &mut ModuleInstance,
    _state: &mut PanelState,
    ink: Color32,
) -> Vec<usize> {
    let colors = &cx.theme.colors;
    let px = 1.0 / cx.m.ppp();
    let painter = ui.painter();
    painter.rect_filled(r, 4.0 * px, colors.control_fill.to_color32());
    painter.hline(r.x_range(), r.center().y, Stroke::new(px, colors.background.to_color32()));

    let inner = r.shrink(3.0 * px);
    if inner.width() <= 2.0 || inner.height() <= 2.0 {
        return Vec::new();
    }

     let slab_arc = live_analysis(ui, module.instance_id);
let slab = slab_arc.as_deref().unwrap_or(&[]);
    if slab.len() >= SCOPE_SAMPLES {
        let samples = &slab[0..SCOPE_SAMPLES];
        let points: Vec<Pos2> = samples
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let t = i as f32 / (SCOPE_SAMPLES - 1) as f32;
                let x = inner.left() + t * inner.width();
                let y = inner.center().y - s.clamp(-1.0, 1.0) * (inner.height() * 0.45);
                pos2(x, y)
            })
            .collect();
        painter.add(Shape::line(points, Stroke::new(1.6 * px, ink)));
        if slab.len() >= ANALYSIS_LEN && slab[T_ACTIVE] > 0.5 {
            ui.ctx().request_repaint();
        }
    }
    Vec::new()
                            }
fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1} kHz", hz / 1000.0)
    } else {
        format!("{:.0} Hz", hz)
    }
}

fn draw_wave_a(
    ui: &mut Ui,
    cx: &UiCtx,
    _id: Id,
    r: Rect,
    module: &mut ModuleInstance,
    _state: &mut PanelState,
    ink: Color32,
) -> Vec<usize> {
    draw_wave_for(
        ui,
        cx,
        r,
        module,
        0usize,
        param::OSC_A_TABLE,
        param::OSC_BASE[0usize] + param::WT_POS,
        T_WT_A,
        T_SYNC_A,
        Some(T_FM),
        ink,
    );
    Vec::new()
}

fn draw_wave_b(
    ui: &mut Ui,
    cx: &UiCtx,
    _id: Id,
    r: Rect,
    module: &mut ModuleInstance,
    _state: &mut PanelState,
    ink: Color32,
) -> Vec<usize> {
    draw_wave_for(
        ui,
        cx,
        r,
        module,
        1usize,
        param::OSC_B_TABLE,
        param::OSC_BASE[1usize] + param::WT_POS,
        T_WT_B,
        T_SYNC_B,
        None,
        ink,
    );
    Vec::new()
}

#[allow(clippy::too_many_arguments)]
fn draw_wave_for(
    ui: &mut Ui,
    cx: &UiCtx,
    r: Rect,
    module: &ModuleInstance,
    osc: usize,
    table_param: usize,
    wt_param: usize,
    t_wt: usize,
    t_sync: usize,
    t_fm: Option<usize>,
    ink: Color32,
) {
    let colors = &cx.theme.colors;
    let px = 1.0 / cx.m.ppp();
    let painter = ui.painter();
    painter.rect_filled(r, 4.0 * px, colors.control_fill.to_color32());
    painter.hline(r.x_range(), r.center().y, Stroke::new(px, colors.background.to_color32()));

     let slab_arc = live_analysis(ui, module.instance_id);
 let slab = slab_arc.as_deref().unwrap_or(&[]);
    let active = slab.len() >= ANALYSIS_LEN && slab[T_ACTIVE] > 0.5;

    let table_idx = choice(
        module.params.get(table_param).copied().unwrap_or(0.0),
        TABLE_MENU_COUNT,
    );
    let base_wt = module.params.get(wt_param).copied().unwrap_or(0.0);
    let wt_pos = if active { slab[t_wt] } else { base_wt };

    let mut samples = [0.0f32; 128];
    wave_preview(osc, table_idx, wt_pos, &mut samples);

    let inner = r.shrink(6.0 * px);
    if inner.width() <= 2.0 || inner.height() <= 2.0 {
        return;
    }

    let points: Vec<Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let t = i as f32 / (samples.len() - 1) as f32;
            let x = inner.left() + t * inner.width();
            let y = inner.center().y - s.clamp(-1.0, 1.0) * (inner.height() * 0.46);
            pos2(x, y)
        })
        .collect();

    painter.add(Shape::line(points, Stroke::new(2.0 * px, ink)));

    let sync_val = if active {
        slab[t_sync]
    } else {
        let p = if osc == 0 {
            param::OSC_A_SYNC
        } else {
            param::OSC_B_SYNC
        };
        module.params.get(p).copied().unwrap_or(0.0)
    };
    let ratio = 1.0 + 15.0 * sync_val * sync_val;
    let mut hud = format!("WT {:.0}%  Sync {:.2}x", wt_pos * 100.0, ratio);
    if let Some(fm_idx) = t_fm {
        let fm_val = if active {
            slab[fm_idx]
        } else {
            module.params.get(param::OSC_A_FM).copied().unwrap_or(0.0)
        };
        hud.push_str(&format!("  FM {:.0}%", fm_val * 100.0));
    }
    let font = cx.m.font_id(FontSize::Pt8);
    painter.text(
        pos2(inner.left() + 2.0 * px, inner.top()),
        Align2::LEFT_TOP,
        hud,
        font,
        colors.text.to_color32().gamma_multiply(0.85),
    );
    if active {
        ui.ctx().request_repaint();
    }
    }
fn draw_filter_curve(
    ui: &mut Ui,
    cx: &UiCtx,
    _id: Id,
    r: Rect,
    module: &mut ModuleInstance,
    _state: &mut PanelState,
    ink: Color32,
) -> Vec<usize> {
    let colors = &cx.theme.colors;
    let px = 1.0 / cx.m.ppp();
    let painter = ui.painter();
    painter.rect_filled(r, 4.0 * px, colors.control_fill.to_color32());

    let inner = r.shrink(6.0 * px);
    if inner.width() <= 4.0 || inner.height() <= 4.0 {
        return Vec::new();
    }

    let grid = Stroke::new(px, colors.background.to_color32());
    for g in [0.25, 0.5, 0.75] {
        painter.hline(inner.x_range(), inner.top() + g * inner.height(), grid);
        painter.vline(inner.left() + g * inner.width(), inner.y_range(), grid);
    }

     let slab_arc = live_analysis(ui, module.instance_id);
 let slab = slab_arc.as_deref().unwrap_or(&[]);
    let active = slab.len() >= ANALYSIS_LEN && slab[T_ACTIVE] > 0.5;

    let kind = FilterKind::from_param(module.params.get(param::FILTER_TYPE).copied().unwrap_or(0.0));
    let base_cut = module.params.get(param::CUTOFF).copied().unwrap_or(0.85);
    let base_res = module.params.get(param::RESONANCE).copied().unwrap_or(0.15);

    let cut = if active { slab[T_CUTOFF] } else { base_cut };
    let res = if active { slab[T_RES] } else { base_res };
    let drive = module.params.get(param::FILTER_DRIVE).copied().unwrap_or(0.0);

    let fc = 20.0 * 2.0f32.powf(10.0 * cut);
    let q = 0.5 * 20.0f32.powf(res);

    let steps = 96usize;
    let mut points = Vec::with_capacity(steps);
    for i in 0..steps {
        let t = i as f32 / (steps - 1) as f32;
        let f = 20.0 * 2.0f32.powf(10.0 * t);
        let w = (f / fc).max(1e-4);
        let w2 = w * w;
        let denom = ((1.0 - w2).powi(2) + (w / q).powi(2)).sqrt().max(1e-4);

        let mag = match kind {
            FilterKind::Lp12 => 1.0 / denom,
            FilterKind::Lp24 => (1.0 / denom).powi(2),
            FilterKind::Hp12 => w2 / denom,
            FilterKind::Hp24 => (w2 / denom).powi(2),
            FilterKind::Bp12 => (w / q) / denom,
            FilterKind::Notch => ((1.0 - w2).abs() / denom).max(0.05),
            FilterKind::Comb => {
                let ripples = (PI * f / fc).cos().abs();
                0.35 + (0.25 + 0.72 * res) * ripples
            }
        };

        let db = (20.0 * mag.max(1e-3).log10() + drive * 6.0).clamp(-36.0, 24.0);
        let norm_y = ((db + 36.0) / 60.0).clamp(0.0, 1.0);
        let x = inner.left() + t * inner.width();
        let y = inner.bottom() - norm_y * inner.height();
        points.push(pos2(x, y));
    }

    if active && (cut - base_cut).abs() > 0.005 {
        let base_x = inner.left() + base_cut.clamp(0.0, 1.0) * inner.width();
        painter.vline(
            base_x,
            inner.y_range(),
            Stroke::new(px, colors.text.to_color32().gamma_multiply(0.35)),
        );
    }

    painter.add(Shape::line(points, Stroke::new(2.0 * px, ink)));

    let dot_x = inner.left() + cut.clamp(0.0, 1.0) * inner.width();
    let dot_y = inner.center().y - res * inner.height() * 0.35;
    let dot_pos = pos2(dot_x, dot_y);
    painter.circle_filled(dot_pos, 4.5 * px, ink);

    let label = format!("{}  Res {:.0}%", format_hz(fc), res * 100.0);
    let font = cx.m.font_id(FontSize::Pt8);
    let (align, text_pos) = if cut > 0.68 {
        (Align2::RIGHT_CENTER, pos2(dot_x - 8.0 * px, dot_y))
    } else {
        (Align2::LEFT_CENTER, pos2(dot_x + 8.0 * px, dot_y))
    };
    painter.text(text_pos, align, label, font, colors.text.to_color32());

    if active {
        ui.ctx().request_repaint();
    }
    Vec::new()
}

fn draw_env_0(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_env_idx(ui, cx, r, m, 0, ink)
}
fn draw_env_1(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_env_idx(ui, cx, r, m, 1, ink)
}
fn draw_env_2(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_env_idx(ui, cx, r, m, 2, ink)
}
fn draw_env_3(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_env_idx(ui, cx, r, m, 3, ink)
}

fn draw_env_idx(
    ui: &mut Ui,
    cx: &UiCtx,
    r: Rect,
    module: &mut ModuleInstance,
    env_idx: usize,
    ink: Color32,
) -> Vec<usize> {
    let colors = &cx.theme.colors;
    let px = 1.0 / cx.m.ppp();
    let painter = ui.painter();
    painter.rect_filled(r, 4.0 * px, colors.control_fill.to_color32());

    let inner = r.shrink(8.0 * px);
    if inner.width() <= 6.0 || inner.height() <= 6.0 {
        return Vec::new();
    }

    let at = param::env_at(env_idx);
    let mut a = module.params.get(at).copied().unwrap_or(0.2);
    let mut d = module.params.get(at + 1).copied().unwrap_or(0.5);
    let mut s = module.params.get(at + 2).copied().unwrap_or(0.8);
    let mut rel = module.params.get(at + 3).copied().unwrap_or(0.4);

    let seg = inner.width() * 0.28;
    let p0 = pos2(inner.left(), inner.bottom());
    let mut p1 = pos2(p0.x + a * seg, inner.top());
    let mut p2 = pos2(p1.x + d * seg, inner.bottom() - s * inner.height());
    let hold_w = inner.width() * 0.16;
    let p3 = pos2(p2.x + hold_w, p2.y);
    let mut p4 = pos2(p3.x + rel * seg, inner.bottom());

    let mut changed = Vec::new();
    let handle_r = 12.0 * px;

    let r1 = ui.interact(
        Rect::from_center_size(p1, vec2(handle_r * 2.0, handle_r * 2.0)),
        ui.id().with(("ds_env_a", env_idx)),
        Sense::drag(),
    );
    if r1.dragged() {
        a = ((r1.interact_pointer_pos().unwrap_or(p1).x - p0.x) / seg).clamp(0.0, 1.0);
        module.params[at] = a;
        changed.push(at);
        p1.x = p0.x + a * seg;
    }

    let r2 = ui.interact(
        Rect::from_center_size(p2, vec2(handle_r * 2.0, handle_r * 2.0)),
        ui.id().with(("ds_env_ds", env_idx)),
        Sense::drag(),
    );
    if r2.dragged() {
        let pos = r2.interact_pointer_pos().unwrap_or(p2);
        d = ((pos.x - p1.x) / seg).clamp(0.0, 1.0);
        s = ((inner.bottom() - pos.y) / inner.height()).clamp(0.0, 1.0);
        module.params[at + 1] = d;
        module.params[at + 2] = s;
        changed.push(at + 1);
        changed.push(at + 2);
        p2 = pos2(p1.x + d * seg, inner.bottom() - s * inner.height());
    }

    let p3_new = pos2(p2.x + hold_w, p2.y);
    let r4 = ui.interact(
        Rect::from_center_size(p4, vec2(handle_r * 2.0, handle_r * 2.0)),
        ui.id().with(("ds_env_r", env_idx)),
        Sense::drag(),
    );
    if r4.dragged() {
        rel = ((r4.interact_pointer_pos().unwrap_or(p4).x - p3_new.x) / seg).clamp(0.0, 1.0);
        module.params[at + 3] = rel;
        changed.push(at + 3);
        p4.x = p3_new.x + rel * seg;
    }

    painter.add(Shape::line(
        vec![p0, p1, p2, p3_new, p4],
        Stroke::new(2.0 * px, ink),
    ));
    for pt in [p1, p2, p4] {
        painter.circle_filled(pt, 5.0 * px, ink);
        painter.circle_stroke(pt, 5.0 * px, Stroke::new(1.5 * px, colors.text.to_color32()));
    }
    changed
            }
fn draw_lfo_0(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_lfo_idx(ui, cx, r, m, 0, ink)
}
fn draw_lfo_1(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_lfo_idx(ui, cx, r, m, 1, ink)
}
fn draw_lfo_2(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_lfo_idx(ui, cx, r, m, 2, ink)
}
fn draw_lfo_3(ui: &mut Ui, cx: &UiCtx, _id: Id, r: Rect, m: &mut ModuleInstance, _s: &mut PanelState, ink: Color32) -> Vec<usize> {
    draw_lfo_idx(ui, cx, r, m, 3, ink)
}

fn draw_lfo_idx(
    ui: &mut Ui,
    cx: &UiCtx,
    r: Rect,
    module: &mut ModuleInstance,
    lfo_idx: usize,
    ink: Color32,
) -> Vec<usize> {
    let colors = &cx.theme.colors;
    let px = 1.0 / cx.m.ppp();
    let painter = ui.painter();
    painter.rect_filled(r, 4.0 * px, colors.control_fill.to_color32());

    let inner = r.shrink(8.0 * px);
    if inner.width() <= 6.0 || inner.height() <= 6.0 {
        return Vec::new();
    }

    let grid = Stroke::new(px, colors.background.to_color32());
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.vline(inner.left() + f * inner.width(), inner.y_range(), grid);
        painter.hline(inner.x_range(), inner.top() + f * inner.height(), grid);
    }

    let base = param::curve_at(lfo_idx);
    let end = (base + CURVE_PARAMS).min(module.params.len());
    let mut curve = if end > base {
        LfoCurve::from_params(&module.params[base..end])
    } else {
        CurvePreset::Triangle.curve()
    };

    let mut changed = Vec::new();
    let handle_r = 11.0 * px;
    let mut dragged_any = false;
    let count = curve.len();

    // 1. Main vertex handles (drag to move, double-click to remove)
    for i in 0..count {
        let (px_norm, py_norm) = curve.point(i);
        let pt_pos = pos2(
            inner.left() + px_norm * inner.width(),
            inner.bottom() - py_norm * inner.height(),
        );
        let resp = ui.interact(
            Rect::from_center_size(pt_pos, vec2(handle_r * 2.0, handle_r * 2.0)),
            ui.id().with(("ds_lfo_pt", lfo_idx, i)),
            Sense::click_and_drag(),
        );
        if resp.double_clicked() && curve.remove(i) {
            dragged_any = true;
            break;
        }
        if resp.dragged() {
            let pos = resp.interact_pointer_pos().unwrap_or(pt_pos);
            let nx = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0);
            let ny = ((inner.bottom() - pos.y) / inner.height()).clamp(0.0, 1.0);
            curve.move_point(i, nx, ny);
            dragged_any = true;
        }
    }

    // 2. Translucent midpoint bend handles between vertices (drag up/down for tension curve, double-click to reset)
    let bend_r = 9.0 * px;
    for i in 0..curve.len().saturating_sub(1) {
        let (x0, y0) = curve.point(i);
        let (x1, y1) = curve.point(i + 1);
        if (x1 - x0).abs() < 0.02 {
            continue;
        }
        let mid_x = 0.5 * (x0 + x1);
        let mid_y = curve.value(mid_x);
        let mid_pos = pos2(
            inner.left() + mid_x * inner.width(),
            inner.bottom() - mid_y * inner.height(),
        );
        let resp = ui.interact(
            Rect::from_center_size(mid_pos, vec2(bend_r * 2.0, bend_r * 2.0)),
            ui.id().with(("ds_lfo_bend", lfo_idx, i)),
            Sense::click_and_drag(),
        );
        if resp.double_clicked() {
            curve.set_bend(i, 0.0);
            dragged_any = true;
        } else if resp.dragged() {
            let dy = -resp.drag_delta().y / (inner.height() * 0.35);
            let dir = if y1 >= y0 { 1.0 } else { -1.0 };
            curve.set_bend(i, curve.bend(i) + dy * dir);
            dragged_any = true;
        }
    }

    // 3. Double-click on empty background to insert a new vertex
    let bg_resp = ui.interact(inner, ui.id().with(("ds_lfo_bg", lfo_idx)), Sense::click());
    if bg_resp.double_clicked()
        && curve.len() < MAX_POINTS
        && let Some(pos) = bg_resp.interact_pointer_pos()
    {
        let nx = ((pos.x - inner.left()) / inner.width()).clamp(0.02, 0.98);
        let ny = ((inner.bottom() - pos.y) / inner.height()).clamp(0.0, 1.0);
        if curve.insert(nx, ny).is_some() {
            dragged_any = true;
        }
    }

    if dragged_any {
        let encoded = curve.to_params();
        for (offset, &val) in encoded.iter().enumerate() {
            let idx = base + offset;
            if idx < module.params.len() {
                module.params[idx] = val;
                changed.push(idx);
            }
        }
    }

    // Draw curve line
    let steps = 96usize;
    let pts: Vec<Pos2> = (0..steps)
        .map(|s| {
            let t = s as f32 / (steps - 1) as f32;
            let y = curve.value(t);
            pos2(inner.left() + t * inner.width(), inner.bottom() - y * inner.height())
        })
        .collect();
    painter.add(Shape::line(pts, Stroke::new(2.0 * px, ink)));

    // Draw translucent tension handles on segments
    let ghost_fill = ink.gamma_multiply(0.35);
    let ghost_stroke = Stroke::new(1.2 * px, colors.text.to_color32().gamma_multiply(0.55));
    for i in 0..curve.len().saturating_sub(1) {
        let (x0, _) = curve.point(i);
        let (x1, _) = curve.point(i + 1);
        if (x1 - x0).abs() < 0.02 {
            continue;
        }
        let mid_x = 0.5 * (x0 + x1);
        let mid_y = curve.value(mid_x);
        let mid_pos = pos2(
            inner.left() + mid_x * inner.width(),
            inner.bottom() - mid_y * inner.height(),
        );
        painter.circle_filled(mid_pos, 3.8 * px, ghost_fill);
        painter.circle_stroke(mid_pos, 3.8 * px, ghost_stroke);
    }

    // Draw main vertex handles
    for i in 0..curve.len() {
        let (px_norm, py_norm) = curve.point(i);
        let pt = pos2(
            inner.left() + px_norm * inner.width(),
            inner.bottom() - py_norm * inner.height(),
        );
        painter.circle_filled(pt, 4.5 * px, ink);
        painter.circle_stroke(pt, 4.5 * px, Stroke::new(1.5 * px, colors.text.to_color32()));
    }

    changed
}

#[cfg(test)]
pub const REFERENCE: Reference = Reference {
    size: [16.96, 16.5],
    settings: &[],
    cases: &[],
};
        
