use eframe::egui::{Color32, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use honeydew_audio::dewserum::modulation::{
    CURVE_PARAMS, CurvePreset, LfoCurve, MAX_POINTS, lfo_preview,
};
use honeydew_audio::dewserum::param;
use honeydew_audio::dewserum::wavetable::wave_preview;

use crate::panels::build::*;
use crate::ui::{UiCtx, widgets};

const LFO_SYNCED: Show = Show::Item(30, 1);

pub fn panel() -> Option<Panel> {
    Some(Panel::new(vstack([
        tabs(0, "OSC A · OSC B · FILTER · LFO · MATRIX · MASTER").h(Size::U(1.25)),
        osc_page(10000, param::OSC_BASE[0], draw_wave_a).show(Show::Tab(0)),
        osc_page(20000, param::OSC_BASE[1], draw_wave_b).show(Show::Tab(1)),
        vstack([
            hstack([
                list(31100, "LP · HP · BP", Bar::Bottom)
                    .p(param::FILTER_TYPE)
                    .w(Size::Percent(30.0)),
                knob(31200, "Cutoff").p(param::CUTOFF),
                knob(31300, "Resonance").p(param::RESONANCE),
                knob(31400, "Key Track").p(param::KEY_TRACK),
            ])
            .h(Size::U(2.8)),
            hstack([
                v_slider(32100, "Env1 Atk").p(param::ENV1),
                v_slider(32200, "Env1 Dec").p(param::ENV1 + 1),
                v_slider(32300, "Env1 Sus").p(param::ENV1 + 2),
                v_slider(32400, "Env1 Rel").p(param::ENV1 + 3),
                separator(32500),
                v_slider(32600, "Env2 Atk").p(param::ENV2),
                v_slider(32700, "Env2 Dec").p(param::ENV2 + 1),
                v_slider(32800, "Env2 Sus").p(param::ENV2 + 2),
                v_slider(32900, "Env2 Rel").p(param::ENV2 + 3),
            ]),
        ])
        .show(Show::Tab(2)),
        hstack([
            display(41000)
                .margins(0.2, 0.2, 0.2, 0.2)
                .w(Size::Flex(2.4))
                .draw(draw_lfo_editor),
            vstack([
                list(42100, "Hertz · Sync", Bar::Bottom)
                    .p(param::LFO_MODE)
                    .h(Size::U(1.3)),
                knob(42200, "Rate")
                    .p(param::LFO_RATE)
                    .show(Show::Not(&LFO_SYNCED)),
                spinner(42300, "Sync Beat")
                    .p(param::LFO_BEAT)
                    .items_per_column(5)
                    .show(LFO_SYNCED),
            ])
            .w(Size::Flex(1.0)),
        ])
        .show(Show::Tab(3)),
        vstack([
            matrix_slot_row(51000, 0),
            matrix_slot_row(52000, 1),
            matrix_slot_row(53000, 2),
            matrix_slot_row(54000, 3),
        ])
        .show(Show::Tab(4)),
        hstack([
            knob(61000, "Master").p(param::MASTER).w(Size::Flex(1.3)),
            spinner(62000, "Voices").p(param::VOICES),
            spacer(63000),
        ])
        .show(Show::Tab(5)),
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
        .h(Size::U(3.2)),
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

fn matrix_slot_row(id_base: u32, slot: usize) -> Part {
    let at = param::MATRIX + slot * 3;
    hstack([
        spinner(id_base + 10, "Source").p(at),
        spinner(id_base + 20, "Dest").p(at + 1),
        h_slider(id_base + 30, "Depth").p(at + 2).w(Size::Flex(1.4)),
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
    draw_wave_for(ui, cx, r, module, param::OSC_BASE[0] + param::WT_POS, ink);
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
    draw_wave_for(ui, cx, r, module, param::OSC_BASE[1] + param::WT_POS, ink);
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
    _state: &mut PanelState,
    ink: Color32,
) -> Vec<usize> {
    let (u, px) = (cx.m.u(1.0), 1.0 / cx.m.ppp());
    let colors = &cx.theme.colors;
    let fill = colors.control_fill.to_color32();
    let bg = colors.background.to_color32();
    let text_col = colors.text.to_color32();

    let mut changed = Vec::new();
    let curve_start = param::LFO_CURVE;
    let curve_end = curve_start + CURVE_PARAMS;
    if module.params.len() < curve_end {
        return changed;
    }

    let mut curve = LfoCurve::from_params(&module.params[curve_start..curve_end]);
    let mut dirty = false;

    let bar_h = 0.85 * u;
    let bar_rect = Rect::from_min_size(r.min, vec2(r.width(), bar_h));
    let plot_rect = Rect::from_min_max(pos2(r.left(), r.top() + bar_h + 4.0 * px), r.max);

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
            ..widgets::Button::new(id.with(("lfo_preset", idx)), br, CurvePreset::NAMES[idx], false, colors)
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

    let resp = ui.interact(plot_rect, id.with("lfo_canvas"), Sense::click_and_drag());
    let drag_id = id.with(("lfo_drag", module.instance_id));
    let mut active_drag: Option<LfoDrag> = ui.data(|d| d.get_temp(drag_id));

    let hit_radius = 0.55 * u;

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
    } else if resp.is_pointer_button_down_on() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if active_drag.is_none() {
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
                .circle_stroke(to_screen(xm, ym), 3.5 * px, Stroke::new(1.5 * px, text_col));
        }
    }

    for i in 0..curve.len() {
        let (px_n, py_n) = curve.point(i);
        let center = to_screen(px_n, py_n);
        ui.painter().circle_filled(center, 5.5 * px, ink);
        ui.painter()
            .circle_stroke(center, 5.5 * px, Stroke::new(1.5 * px, bg));
    }

    changed
}
