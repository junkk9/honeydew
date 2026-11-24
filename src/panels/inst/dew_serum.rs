use eframe::egui::{Color32, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use honeydew_audio::dewserum::modulation::{
    CURVE_PARAMS, CurvePreset, LfoCurve, MAX_POINTS, lfo_preview,
};
use honeydew_audio::dewserum::wavetable::wave_preview;
use honeydew_audio::dewserum::{ENV_COUNT, LFO_COUNT, param};

use crate::panels::build::*;
use crate::ui::{UiCtx, widgets};

const LFO_1_SYNCED: Show = Show::Item(param::lfo_at(0usize) + param::LFO_MODE, 1);
const LFO_2_SYNCED: Show = Show::Item(param::lfo_at(1usize) + param::LFO_MODE, 1);
const LFO_3_SYNCED: Show = Show::Item(param::lfo_at(2usize) + param::LFO_MODE, 1);
const LFO_4_SYNCED: Show = Show::Item(param::lfo_at(3usize) + param::LFO_MODE, 1);

const LFO_SYNC_SHOWS: [Show; LFO_COUNT] = [LFO_1_SYNCED, LFO_2_SYNCED, LFO_3_SYNCED, LFO_4_SYNCED];

pub fn panel() -> Option<Panel> {
    Some(Panel::new(vstack([
        tabs(0, "OSC A · OSC B · FILTER · ENV · LFO · MATRIX · MASTER").h(Size::U(1.35)),
        osc_page(10000, param::OSC_BASE[0usize], draw_wave_a).show(Show::Tab(0)),
        osc_page(20000, param::OSC_BASE[1usize], draw_wave_b).show(Show::Tab(1)),
        vstack([
            list(31100, "LP · HP · BP", Bar::Bottom)
                .p(param::FILTER_TYPE)
                .h(Size::U(1.4)),
            hstack([
                knob(31200, "Cutoff").p(param::CUTOFF),
                knob(31300, "Resonance").p(param::RESONANCE),
                knob(31400, "Key Track").p(param::KEY_TRACK),
            ]),
        ])
        .show(Show::Tab(2)),
        env_page().show(Show::Tab(3)),
        lfo_page().show(Show::Tab(4)),
        matrix_page().show(Show::Tab(5)),
        hstack([
            knob(61000, "Master").p(param::MASTER).w(Size::Flex(1.3)),
            spinner(62000, "Voices").p(param::VOICES),
            spacer(63000),
        ])
        .show(Show::Tab(6)),
    ])))
}

fn osc_page(id_base: u32, base: usize, draw_fn: Draw) -> Part {
    vstack([
        hstack([
            display(id_base + 100)
                .margins(0.2, 0.2, 0.2, 0.2)
                .w(Size::Flex(1.6))
                .draw(draw_fn),
            knob(id_base + 200, "WT Pos").p(base + param::WT_POS),
            knob(id_base + 300, "Level").p(base + param::LEVEL),
        ])
        .h(Size::U(4.0)),
        hstack([
            spinner(id_base + 400, "Pitch").p(base + param::PITCH),
            knob(id_base + 500, "Fine").p(base + param::FINE),
            spinner(id_base + 600, "Unison").p(base + param::UNISON),
            v_slider(id_base + 700, "Detune").p(base + param::DETUNE),
            v_slider(id_base + 800, "Blend").p(base + param::BLEND),
            v_slider(id_base + 900, "Width").p(base + param::WIDTH),
        ]),
    ])
}

fn env_page() -> Part {
    vstack([
        display(35000)
            .margins(0.2, 0.2, 0.2, 0.1)
            .h(Size::U(5.4))
            .draw(draw_env_editor),
        vstack(std::array::from_fn::<Part, ENV_COUNT, _>(|e| {
            let at = param::env_at(e);
            let id_b = 36000 + (e as u32) * 100;
            hstack([
                v_slider(id_b + 1, "Attack").p(at),
                v_slider(id_b + 2, "Decay").p(at + 1),
                v_slider(id_b + 3, "Sustain").p(at + 2),
                v_slider(id_b + 4, "Release").p(at + 3),
            ])
            .show(Show::Flag("ds_env", e))
                0 => |_, s| active_env(s) == 0,
                1 => |_, s| active_env(s) == 1,
                2 => |_, s| active_env(s) == 2,
                _ => |_, s| active_env(s) == 3,
        })),
    ])
}

fn lfo_page() -> Part {
    hstack([
        display(41000)
            .margins(0.2, 0.2, 0.2, 0.2)
            .w(Size::Flex(2.4))
            .draw(draw_lfo_editor),
        vstack(std::array::from_fn::<Part, LFO_COUNT, _>(|l| {
            let at = param::lfo_at(l);
            let id_b = 42000 + (l as u32) * 100;
            let sync_show = LFO_SYNC_SHOWS[l];
            vstack([
                list(id_b + 10, "Hertz · Sync", Bar::Bottom)
                    .p(at + param::LFO_MODE)
                    .h(Size::U(1.3)),
                list(id_b + 15, "Trig · Env · Free", Bar::Bottom)
                    .p(at + param::LFO_TRIG)
                    .h(Size::U(1.3)),
                knob(id_b + 20, "Rate")
                    .p(at + param::LFO_RATE)
                    .show(Show::Not(match l {
                        0 => &LFO_1_SYNCED,
                        1 => &LFO_2_SYNCED,
                        2 => &LFO_3_SYNCED,
                        _ => &LFO_4_SYNCED,
                    })),
                spinner(id_b + 30, "Sync Beat")
                    .p(at + param::LFO_BEAT)
                    .items_per_column(5)
                    .show(sync_show),
            ])
                    .show(Show::Flag("ds_lfo", l))
        }))
        .w(Size::Flex(1.0)),
    ])
}

fn matrix_page() -> Part {
    vstack([
        display(50500)
            .margins(0.2, 0.1, 0.2, 0.1)
            .h(Size::U(1.1))
            .draw(draw_matrix_pager),
        vstack([
            matrix_slot_row(51000, 0),
            matrix_slot_row(52000, 1),
            matrix_slot_row(53000, 2),
            matrix_slot_row(54000, 3),
        ])
        .show(Show::Flag("ds_mat_page", 0)),
        vstack([
            matrix_slot_row(55000, 4),
            matrix_slot_row(56000, 5),
            matrix_slot_row(57000, 6),
            matrix_slot_row(58000, 7),
        ])
        .show(Show::Flag("ds_mat_page", 1)),
    ])
}

fn matrix_slot_row(id_base: u32, slot: usize) -> Part {
    let at = param::MATRIX + slot * 3;
    hstack([
        spinner(id_base + 10, "").p(at).w(Size::Flex(1.0)),
        spinner(id_base + 20, "").p(at + 1).w(Size::Flex(1.0)),
        h_slider(id_base + 30, "Depth").p(at + 2).w(Size::Flex(1.3)),
    ])
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
    draw_wave_for(ui, cx, r, module, param::OSC_BASE[0usize] + param::WT_POS, ink);
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
    draw_wave_for(ui, cx, r, module, param::OSC_BASE[1usize] + param::WT_POS, ink);
    Vec::new()
}

fn draw_wave_for(
    ui: &mut Ui,
    cx: &UiCtx,
    r: Rect,
    module: &ModuleInstance,
    wt_param: usize,
    ink: Color32,
) {
    let colors = &cx.theme.colors;
    let px = 1.0 / cx.m.ppp();
    let painter = ui.painter();
    painter.rect_filled(r, 4.0 * px, colors.control_fill.to_color32());
    painter.hline(r.x_range(), r.center().y, Stroke::new(px, colors.background.to_color32()));

    let wt_pos = module.params.get(wt_param).copied().unwrap_or(0.0);
    let mut samples = [0.0f32; 128];
    wave_preview(wt_pos, &mut samples);

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
}

fn draw_matrix_pager(
    ui: &mut Ui,
    cx: &UiCtx,
    id: Id,
    r: Rect,
    _module: &mut ModuleInstance,
    state: &mut PanelState;
    ink: Color32,
) -> Vec<usize> {
    let px = 1.0 / cx.m.ppp();
    let colors = &cx.theme.colors;
    let fill = colors.control_fill.to_color32();
    let text_col = colors.text.to_color32();
    let mut page = state.flags.get("ds_mat_page").copied().unwrap_or(0).min(1);

    let labels = ["Slots 1–4", "Slots 5–8"];
    let btn_w = r.width() / 2.0;
    for (idx, label) in labels.iter().enumerate() {
        let br = Rect::from_min_size(
            pos2(r.left() + idx as f32 * btn_w, r.top()),
            vec2(btn_w - 3.0 * px, r.height()),
        );
        let active = page == idx;
        let b = widgets::Button {
            fill_on: ink,
            fill_off: fill,
            text_on: fill,
            text_off: Some(text_col),
            ..widgets::Button::new(id.with(("mat_pg", idx)), br, *label, active, colors)
        };
        if widgets::button(ui, cx, &b, active).clicked() {
            page = idx;
            state.flags.insert("ds_mat_page", page);
        }
    }
    Vec::new()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvDrag {
    Attack,
    DecaySustain,
    Release,
}

fn draw_env_editor(
    ui: &mut Ui,
    cx: &UiCtx,
    id: Id,
    r: Rect,
    module: &mut ModuleInstance,
    state: &mut PanelState;
    ink: Color32,
) -> Vec<usize> {
    let (u, px) = (cx.m.u(1.0), 1.0 / cx.m.ppp());
    let colors = &cx.theme.colors;
    let fill = colors.control_fill.to_color32();
    let bg = colors.background.to_color32();
    let text_col = colors.text.to_color32();
    let mut changed = Vec::new();

    let mut cur_env = state.flags.get("ds_env").copied().unwrap_or(0).min(ENV_COUNT - 1);
    let bar_h = 0.95 * u;
    let bar_rect = Rect::from_min_size(r.min, vec2(r.width(), bar_h));
    let plot_rect = Rect::from_min_max(pos2(r.left(), r.top() + bar_h + 4.0 * px), r.max);

    let env_names = ["ENV 1 (Amp)", "ENV 2", "ENV 3", "ENV 4"];
    let btn_w = bar_rect.width() / ENV_COUNT as f32;
    for (idx, name) in env_names.iter().enumerate() {
        let br = Rect::from_min_size(
            pos2(bar_rect.left() + idx as f32 * btn_w, bar_rect.top()),
            vec2(btn_w - 3.0 * px, bar_rect.height()),
        );
        let active = cur_env == idx;
        let b = widgets::Button {
            fill_on: ink,
            fill_off: fill,
            text_on: fill,
            text_off: Some(text_col),
            ..widgets::Button::new(id.with(("env_sel", idx)), br, *name, active, colors)
        };
        if widgets::button(ui, cx, &b, active).clicked() {
            cur_env = idx;
            state.flags.insert("ds_env", cur_env);
        }
    }

    let at = param::env_at(cur_env);
    if module.params.len() < at + 4 {
        return changed;
    }

    ui.painter().rect_filled(plot_rect, 4.0 * px, fill);
    let inner = plot_rect.shrink(10.0 * px);
    if inner.width() <= 6.0 || inner.height() <= 6.0 {
        return changed;
    }

    let mut atk = module.params[at];
    let mut dec = module.params[at + 1];
    let mut sus = module.params[at + 2];
    let mut rel = module.params[at + 3];

    let seg_w = inner.width() / 3.2;
    let p0 = pos2(inner.left(), inner.bottom());
    let p1 = pos2(inner.left() + atk * seg_w, inner.top());
    let p2 = pos2(p1.x + dec * seg_w, inner.bottom() - sus * inner.height());
    let p3 = pos2(p2.x + 0.2 * seg_w, p2.y);
    let p4 = pos2(p3.x + rel * seg_w, inner.bottom());

    let resp = ui.interact(plot_rect, id.with("env_canvas"), Sense::drag());
    let drag_id = id.with(("env_drag", module.instance_id));
    let mut drag: Option<EnvDrag> = ui.data(|d| d.get_temp(drag_id));
    let pointer_down = ui.input(|i| i.pointer.primary_down());
    let pointer_pos = ui.input(|i| i.pointer.latest_pos());

    if pointer_down {
        if let Some(pos) = pointer_pos {
            if drag.is_none() && resp.hovered() {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                let hit = 1.15 * u;
                let pts = [
                    (EnvDrag::Attack, p1.distance(origin)),
                    (EnvDrag::DecaySustain, p2.distance(origin)),
                    (EnvDrag::Release, p4.distance(origin)),
                ];
                if let Some((best, d)) = pts.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)) {
                    if d <= hit {
                        drag = Some(best);
                    }
                }
            }
            match drag {
                Some(EnvDrag::Attack) => {
                    atk = ((pos.x - inner.left()) / seg_w).clamp(0.0, 1.0);
                }
                Some(EnvDrag::DecaySustain) => {
                    dec = ((pos.x - p1.x) / seg_w).clamp(0.0, 1.0);
                    sus = ((inner.bottom() - pos.y) / inner.height()).clamp(0.0, 1.0);
                }
                Some(EnvDrag::Release) => {
                    rel = ((pos.x - p3.x) / seg_w).clamp(0.0, 1.0);
                }
                None => {}
            }
        }
    } else {
        drag = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, drag));

    let next = [atk, dec, sus, rel];
    for (k, &val) in next.iter().enumerate() {
        let idx = at + k;
        if (module.params[idx] - val).abs() > 1e-5 {
            module.params[idx] = val;
            changed.push(idx);
        }
    }

    let p1 = pos2(inner.left() + atk * seg_w, inner.top());
    let p2 = pos2(p1.x + dec * seg_w, inner.bottom() - sus * inner.height());
    let p3 = pos2(p2.x + 0.2 * seg_w, p2.y);
    let p4 = pos2(p3.x + rel * seg_w, inner.bottom());

    ui.painter()
        .add(Shape::line(vec![p0, p1, p2, p3, p4], Stroke::new(2.2 * px, ink)));
    for pt in [p1, p2, p4] {
        ui.painter().circle_filled(pt, 6.0 * px, ink);
        ui.painter().circle_stroke(pt, 6.0 * px, Stroke::new(1.5 * px, bg));
    }

    changed
    }
#[derive(Clone, Copy, Debug)]
enum LfoDrag {
    Point(usize),
    Bend(usize, f32, Pos2),
}

fn draw_lfo_editor(
    ui: &mut Ui,
    cx: &UiCtx,
    id: Id,
    r: Rect,
    module: &mut ModuleInstance,
    state: &mut PanelState;
    ink: Color32,
) -> Vec<usize> {
    let (u, px) = (cx.m.u(1.0), 1.0 / cx.m.ppp());
    let colors = &cx.theme.colors;
    let fill = colors.control_fill.to_color32();
    let bg = colors.background.to_color32();
    let text_col = colors.text.to_color32();

    let mut changed = Vec::new();
    let mut cur_lfo = state.flags.get("ds_lfo").copied().unwrap_or(0).min(LFO_COUNT - 1);

    let sel_h = 0.85 * u;
    let sel_rect = Rect::from_min_size(r.min, vec2(r.width(), sel_h));
    let bar_h = 0.85 * u;
    let bar_rect = Rect::from_min_size(
        pos2(r.left(), sel_rect.bottom() + 3.0 * px),
        vec2(r.width(), bar_h),
    );
    let plot_rect = Rect::from_min_max(pos2(r.left(), bar_rect.bottom() + 4.0 * px), r.max);

    let lfo_names = ["LFO 1", "LFO 2", "LFO 3", "LFO 4"];
    let lfo_btn_w = sel_rect.width() / LFO_COUNT as f32;
    for (idx, name) in lfo_names.iter().enumerate() {
        let br = Rect::from_min_size(
            pos2(sel_rect.left() + idx as f32 * lfo_btn_w, sel_rect.top()),
            vec2(lfo_btn_w - 2.0 * px, sel_rect.height()),
        );
        let active = cur_lfo == idx;
        let b = widgets::Button {
            fill_on: ink,
            fill_off: fill,
            text_on: fill,
            text_off: Some(text_col),
            ..widgets::Button::new(id.with(("lfo_sel", idx)), br, *name, active, colors)
        };
        if widgets::button(ui, cx, &b, active).clicked() {
            cur_lfo = idx;
            state.flags.insert("ds_lfo", cur_lfo);
        }
    }

    let curve_start = param::curve_at(cur_lfo);
    let curve_end = curve_start + CURVE_PARAMS;
    if module.params.len() < curve_end {
        return changed;
    }

    let mut curve = LfoCurve::from_params(&module.params[curve_start..curve_end]);
    let mut dirty = false;

    let btn_w = bar_rect.width() / CurvePreset::ALL.len() as f32;
    for (idx, &preset) in CurvePreset::ALL.iter().enumerate() {
        let br = Rect::from_min_size(
            pos2(bar_rect.left() + idx as f32 * btn_w, bar_rect.top()),
            vec2(btn_w - 2.0 * px, bar_rect.height()),
        );
        let b = widgets::Button {
            fill_on: ink,
            fill_off: fill,
            text_on: fill,
            text_off: Some(text_col),
            ..widgets::Button::new(
                id.with(("lfo_preset", cur_lfo, idx)),
                br,
                CurvePreset::NAMES[idx],
                false,
                colors,
            )
        };
        if widgets::button(ui, cx, &b, false).clicked() {
            curve = preset.curve();
            dirty = true;
        }
    }

    ui.painter().rect_filled(plot_rect, 4.0 * px, fill);
    let inner = plot_rect.shrink(8.0 * px);
    if inner.width() <= 4.0 || inner.height() <= 4.0 {
        return changed;
    }

    for k in 1..4 {
        let gx = inner.left() + inner.width() * (k as f32 / 4.0);
        ui.painter().vline(gx, inner.y_range(), Stroke::new(px, bg));
    }
    ui.painter()
        .hline(inner.x_range(), inner.center().y, Stroke::new(px, bg));

    let to_screen = |nx: f32, ny: f32| -> Pos2 {
        pos2(
            inner.left() + nx.clamp(0.0, 1.0) * inner.width(),
            inner.bottom() - ny.clamp(0.0, 1.0) * inner.height(),
        )
    };
    let from_screen = |p: Pos2| -> (f32, f32) {
        (
            ((p.x - inner.left()) / inner.width()).clamp(0.0, 1.0),
            ((inner.bottom() - p.y) / inner.height()).clamp(0.0, 1.0),
        )
    };

    let resp = ui.interact(plot_rect, id.with(("lfo_canvas", cur_lfo)), Sense::click_and_drag());
    let drag_id = id.with(("lfo_drag", module.instance_id, cur_lfo));
    let mut active_drag: Option<LfoDrag> = ui.data(|d| d.get_temp(drag_id));

    let hit_radius = 1.15 * u;
    let pointer_down = ui.input(|i| i.pointer.primary_down());
    let pointer_pos = ui.input(|i| i.pointer.latest_pos());

    if resp.double_clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let mut removed = false;
            for i in 1..curve.len().saturating_sub(1) {
                let (px_n, py_n) = curve.point(i);
                if to_screen(px_n, py_n).distance(pos) <= hit_radius {
                    if curve.remove(i) {
                        dirty = true;
                        removed = true;
                    }
                    break;
                }
            }
            if !removed && curve.len() < MAX_POINTS {
                let (nx, ny) = from_screen(pos);
                if curve.insert(nx, ny).is_some() {
                    dirty = true;
                }
            }
        }
        active_drag = None;
    } else if pointer_down {
        if let Some(pos) = pointer_pos {
            if active_drag.is_none() && (resp.hovered() || resp.is_pointer_button_down_on()) {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                let mut best_pt: Option<(usize, f32)> = None;
                for i in 0..curve.len() {
                    let (px_n, py_n) = curve.point(i);
                    let d = to_screen(px_n, py_n).distance(origin);
                    if d <= hit_radius && best_pt.is_none_or(|(_, bd)| d < bd) {
                        best_pt = Some((i, d));
                    }
                }
                if let Some((i, _)) = best_pt {
                    active_drag = Some(LfoDrag::Point(i));
                } else {
                    let mut best_seg: Option<(usize, f32)> = None;
                    for i in 0..curve.len().saturating_sub(1) {
                        let (x0, _) = curve.point(i);
                        let (x1, _) = curve.point(i + 1);
                        let xm = 0.5 * (x0 + x1);
                        let ym = curve.value(xm);
                        let d = to_screen(xm, ym).distance(origin);
                        if d <= hit_radius && best_seg.is_none_or(|(_, bd)| d < bd) {
                            best_seg = Some((i, d));
                        }
                    }
                    if let Some((seg, _)) = best_seg {
                        active_drag = Some(LfoDrag::Bend(seg, curve.bend(seg), origin));
                    }
                }
            }

            match active_drag {
                Some(LfoDrag::Point(i)) => {
                    let (nx, ny) = from_screen(pos);
                    curve.move_point(i, nx, ny);
                    dirty = true;
                }
                Some(LfoDrag::Bend(seg, start_bend, origin)) => {
                    let dy = (origin.y - pos.y) / (inner.height() * 0.45);
                    curve.set_bend(seg, start_bend + dy);
                    dirty = true;
                }
                None => {}
            }
        }
    } else {
        active_drag = None;
    }

    ui.data_mut(|d| d.insert_temp(drag_id, active_drag));

    if dirty {
        let encoded = curve.to_params();
        for (i, &val) in encoded.iter().enumerate() {
            let idx = curve_start + i;
            if (module.params[idx] - val).abs() > 1e-6 {
                module.params[idx] = val;
                changed.push(idx);
            }
        }
        ui.ctx().request_repaint();
    }

    let mut preview = [0.0f32; 128];
    lfo_preview(&module.params[curve_start..curve_end], &mut preview);
    let line_pts: Vec<Pos2> = preview
        .iter()
        .enumerate()
        .map(|(k, &v)| {
            let nx = k as f32 / (preview.len() - 1) as f32;
            let ny = 0.5 * (v + 1.0);
            to_screen(nx, ny)
        })
        .collect();
    ui.painter()
        .add(Shape::line(line_pts, Stroke::new(2.0 * px, ink)));

    for i in 0..curve.len().saturating_sub(1) {
        let (x0, _) = curve.point(i);
        let (x1, _) = curve.point(i + 1);
        if (x1 - x0).abs() > 0.03 {
            let xm = 0.5 * (x0 + x1);
            let ym = curve.value(xm);
            ui.painter()
                .circle_stroke(to_screen(xm, ym), 4.5 * px, Stroke::new(1.5 * px, text_col));
        }
    }

    for i in 0..curve.len() {
        let (px_n, py_n) = curve.point(i);
        let center = to_screen(px_n, py_n);
        ui.painter().circle_filled(center, 6.5 * px, ink);
        ui.painter()
            .circle_stroke(center, 6.5 * px, Stroke::new(1.5 * px, bg));
    }

    changed
}

#[cfg(test)]
pub const REFERENCE: Reference = Reference {
    size: [16.96, 13.0],
    settings: &[],
    cases: &[],
};
            
