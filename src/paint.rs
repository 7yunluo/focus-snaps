// 浮层绘制：画面、遮罩、标注、选区边框与手柄
use gtk::cairo;
use gtk::pango;
use pangocairo::functions::{create_layout, show_layout};

use crate::doc::{Doc, OutputLayer, Stroke, Tool};

/// 选区外压暗的强度
const DIM: f64 = 0.45;
/// 手柄边长（逻辑像素）
pub const HANDLE: f64 = 7.0;
/// 手柄命中半径（逻辑像素）
pub const HANDLE_HIT: f64 = 9.0;

/// 画一个显示器的整块浮层，area 是窗口的实际逻辑尺寸
pub fn paint(cr: &cairo::Context, doc: &mut Doc, out_idx: usize, area: (f64, f64)) {
    let Doc {
        outs,
        sel,
        strokes,
        active,
        tool,
        hover,
        ..
    } = doc;
    let Some(out) = outs.get_mut(out_idx) else {
        return;
    };

    // 画面按物理像素 1:1 采样，避免二次缩放发虚
    let _ = cr.save();
    cr.scale(1.0 / out.sx, 1.0 / out.sy);
    let _ = cr.set_source_surface(&out.phys, 0.0, 0.0);
    let _ = cr.paint();
    let _ = cr.restore();

    let sel = sel
        .filter(|s| s.out == out_idx)
        .map(|s| s.normalized());

    // 悬停的窗口先用它当「洞」，让用户看清将选中哪一块
    let hover_rect = hover
        .as_ref()
        .filter(|(o, _)| *o == out_idx)
        .map(|(_, r)| (r.x0, r.y0, r.x1, r.y1));

    // 选区外（或悬停窗口外）压暗
    cr.set_source_rgba(0.0, 0.0, 0.0, DIM);
    match sel.or(hover_rect) {
        Some((x0, y0, x1, y1)) => {
            let (w, h) = area;
            quad(cr, 0.0, 0.0, w, y0);
            quad(cr, 0.0, y1, w, h - y1);
            quad(cr, 0.0, y0, x0, y1 - y0);
            quad(cr, x1, y0, w - x1, y1 - y0);
            let _ = cr.fill();
        }
        None => {
            let _ = cr.paint();
        }
    }

    let Some((x0, y0, x1, y1)) = sel else {
        // 还没框选时，把指针悬停的窗口勾出来
        if let Some((o, r)) = hover.as_ref() {
            if *o == out_idx {
                draw_hover(cr, r);
            }
        }
        return;
    };

    // 标注只画在选区内
    let _ = cr.save();
    cr.rectangle(x0, y0, x1 - x0, y1 - y0);
    cr.clip();
    for s in strokes.iter() {
        draw_stroke(cr, out, s);
    }
    if let Some(a) = active.as_ref() {
        draw_stroke(cr, out, a);
    }
    let _ = cr.restore();

    draw_frame(cr, (x0, y0, x1, y1), *tool == Tool::Select);
    draw_badge(cr, out, (x0, y0, x1, y1));
}

fn quad(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64) {
    if w > 0.0 && h > 0.0 {
        cr.rectangle(x, y, w, h);
    }
}

/// 悬停窗口的高亮框
fn draw_hover(cr: &cairo::Context, r: &crate::windows::Rect) {
    let _ = cr.save();
    cr.set_source_rgba(1.0, 0.72, 0.55, 0.95);
    cr.set_line_width(2.0);
    cr.rectangle(r.x0, r.y0, r.width(), r.height());
    let _ = cr.stroke();
    let _ = cr.restore();
}

/// 选区边框 + 拖拽手柄
fn draw_frame(cr: &cairo::Context, r: (f64, f64, f64, f64), handles: bool) {
    let (x0, y0, x1, y1) = r;
    let _ = cr.save();
    cr.set_line_width(1.0);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    cr.rectangle(x0 - 0.5, y0 - 0.5, x1 - x0 + 1.0, y1 - y0 + 1.0);
    let _ = cr.stroke();

    cr.set_source_rgba(1.0, 0.72, 0.55, 1.0);
    cr.rectangle(x0 + 0.5, y0 + 0.5, x1 - x0 - 1.0, y1 - y0 - 1.0);
    let _ = cr.stroke();

    if handles {
        cr.set_source_rgba(1.0, 0.72, 0.55, 1.0);
        for (hx, hy) in handle_points(r) {
            cr.rectangle(hx - HANDLE / 2.0, hy - HANDLE / 2.0, HANDLE, HANDLE);
        }
        let _ = cr.fill_preserve();
        cr.set_source_rgba(0.1, 0.08, 0.07, 1.0);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    }
    let _ = cr.restore();
}

/// 八个手柄的中心点：四角 + 四边中点
pub fn handle_points(r: (f64, f64, f64, f64)) -> [(f64, f64); 8] {
    let (x0, y0, x1, y1) = r;
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    [
        (x0, y0),
        (cx, y0),
        (x1, y0),
        (x1, cy),
        (x1, y1),
        (cx, y1),
        (x0, y1),
        (x0, cy),
    ]
}

/// 选区尺寸徽标，显示物理像素
fn draw_badge(cr: &cairo::Context, out: &OutputLayer, r: (f64, f64, f64, f64)) {
    let (x0, y0, x1, y1) = r;
    let wpx = ((x1 - x0) * out.sx).round() as i64;
    let hpx = ((y1 - y0) * out.sy).round() as i64;
    let label = format!("{wpx} × {hpx}");
    let layout = text_layout(cr, &label, 13.0, false);
    let (tw, th) = layout.pixel_size();
    let pad = 6.0;
    let bw = tw as f64 + pad * 2.0;
    let bh = th as f64 + 6.0;
    // 优先放在选区上方，其次下方，都放不下就压进选区内部并保证不出界
    let bx = x0.clamp(2.0, (out.size.0 - bw - 2.0).max(2.0));
    let by = if y0 - bh - 6.0 >= 2.0 {
        y0 - bh - 6.0
    } else if y1 + 6.0 + bh <= out.size.1 - 2.0 {
        y1 + 6.0
    } else {
        (y0 + 6.0).clamp(2.0, (out.size.1 - bh - 2.0).max(2.0))
    };

    let _ = cr.save();
    cr.set_source_rgba(0.08, 0.06, 0.05, 0.82);
    rounded_rect(cr, bx, by, bw, bh, 5.0);
    let _ = cr.fill();
    cr.set_source_rgba(0.96, 0.93, 0.91, 1.0);
    cr.move_to(bx + pad, by + 3.0);
    show_layout(cr, &layout);
    let _ = cr.restore();
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(x + r, y + r, r, std::f64::consts::PI, std::f64::consts::PI * 1.5);
    cr.close_path();
}

/// 建一个指定字号的文字布局
fn text_layout(cr: &cairo::Context, text: &str, px: f64, bold: bool) -> pango::Layout {
    let layout = create_layout(cr);
    let mut desc = pango::FontDescription::from_string(if bold { "Sans Bold" } else { "Sans" });
    desc.set_absolute_size(px * f64::from(pango::SCALE));
    layout.set_font_description(Some(&desc));
    layout.set_text(text);
    layout
}

/// 画一笔标注
pub fn draw_stroke(cr: &cairo::Context, out: &mut OutputLayer, s: &Stroke) {
    let (r, g, b) = s.color;
    match s.tool {
        Tool::Brush | Tool::Highlighter => {
            if s.points.is_empty() {
                return;
            }
            let highlighter = s.tool == Tool::Highlighter;
            let width = if highlighter { s.width * 3.5 } else { s.width };
            let _ = cr.save();
            cr.set_line_cap(cairo::LineCap::Round);
            cr.set_line_join(cairo::LineJoin::Round);
            cr.set_source_rgba(r, g, b, if highlighter { 0.32 } else { 1.0 });
            cr.set_line_width(width);
            let (p0, rest) = s.points.split_first().unwrap();
            cr.move_to(p0.0, p0.1);
            if rest.is_empty() {
                // 单点也画一个圆点
                cr.line_to(p0.0 + 0.01, p0.1);
            } else {
                for p in rest {
                    cr.line_to(p.0, p.1);
                }
            }
            let _ = cr.stroke();
            let _ = cr.restore();
        }
        Tool::Line | Tool::Arrow | Tool::Rect | Tool::Ellipse | Tool::Mosaic | Tool::Crop => {
            if s.points.len() < 2 {
                return;
            }
            let (x0, y0) = s.points[0];
            let (x1, y1) = s.points[1];
            match s.tool {
                Tool::Mosaic => paint_mosaic(cr, out, (x0, y0), (x1, y1)),
                Tool::Crop => {
                    let _ = cr.save();
                    cr.set_source_rgba(r, g, b, 0.9);
                    cr.set_line_width(1.5);
                    cr.set_dash(&[6.0, 4.0], 0.0);
                    cr.rectangle(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs());
                    let _ = cr.stroke();
                    let _ = cr.restore();
                }
                _ => {
                    let _ = cr.save();
                    cr.set_line_cap(cairo::LineCap::Round);
                    cr.set_line_join(cairo::LineJoin::Round);
                    cr.set_source_rgba(r, g, b, 1.0);
                    cr.set_line_width(s.width);
                    match s.tool {
                        Tool::Line => {
                            cr.move_to(x0, y0);
                            cr.line_to(x1, y1);
                            let _ = cr.stroke();
                        }
                        Tool::Arrow => {
                            let angle = (y1 - y0).atan2(x1 - x0);
                            let head = (s.width * 4.0).max(11.0);
                            // 箭杆缩短一点，避免穿透箭头尖端
                            let cut = head * 0.55;
                            cr.move_to(x0, y0);
                            cr.line_to(x1 - cut * angle.cos(), y1 - cut * angle.sin());
                            let _ = cr.stroke();
                            let spread = 0.42;
                            cr.move_to(x1, y1);
                            cr.line_to(
                                x1 - head * (angle - spread).cos(),
                                y1 - head * (angle - spread).sin(),
                            );
                            cr.line_to(
                                x1 - head * 0.62 * (angle).cos(),
                                y1 - head * 0.62 * (angle).sin(),
                            );
                            cr.line_to(
                                x1 - head * (angle + spread).cos(),
                                y1 - head * (angle + spread).sin(),
                            );
                            cr.close_path();
                            let _ = cr.fill();
                        }
                        Tool::Rect => {
                            cr.rectangle(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs());
                            let _ = cr.stroke();
                        }
                        Tool::Ellipse => {
                            let _ = cr.save();
                            cr.translate((x0 + x1) / 2.0, (y0 + y1) / 2.0);
                            cr.scale((x1 - x0).abs() / 2.0, (y1 - y0).abs() / 2.0);
                            cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
                            let _ = cr.restore();
                            let _ = cr.stroke();
                        }
                        _ => {}
                    }
                    let _ = cr.restore();
                }
            }
        }
        Tool::Text => {
            if let Some(text) = s.text.as_ref() {
                let px = (s.width * 5.0).max(13.0);
                let layout = text_layout(cr, text, px, true);
                let (_, th) = layout.pixel_size();
                let _ = cr.save();
                cr.set_source_rgba(r, g, b, 1.0);
                cr.move_to(s.points[0].0, s.points[0].1);
                cr.rel_move_to(0.0, th as f64 * 0.15);
                show_layout(cr, &layout);
                let _ = cr.restore();
            }
        }
        Tool::Number => {
            let n = s.number.unwrap_or(1);
            let radius = (s.width * 2.6).max(9.0);
            let (x, y) = s.points[0];
            let layout = text_layout(cr, &n.to_string(), radius * 1.35, true);
            let (tw, th) = layout.pixel_size();
            let _ = cr.save();
            cr.set_source_rgba(r, g, b, 1.0);
            cr.arc(x, y, radius, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            cr.move_to(
                x - tw as f64 / 2.0,
                y - th as f64 / 2.0,
            );
            show_layout(cr, &layout);
            let _ = cr.restore();
        }
        Tool::Select => {}
    }
}

/// 区域打码：从原画面按块取平均色再填实心方块
fn paint_mosaic(cr: &cairo::Context, out: &mut OutputLayer, a: (f64, f64), b: (f64, f64)) {
    let x0 = a.0.min(b.0).max(0.0);
    let y0 = a.1.min(b.1).max(0.0);
    let x1 = a.0.max(b.0).min(out.size.0);
    let y1 = a.1.max(b.1).min(out.size.1);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let stride = out.phys.stride() as usize;
    let (bw, bh) = (out.phys.width(), out.phys.height());
    let data = match out.phys.data() {
        Ok(d) => d,
        Err(_) => return,
    };
    // 块大小随缩放微调，观感上始终是 12 逻辑像素
    let block = 12.0;
    let step = 3usize;

    let _ = cr.save();
    let mut y = y0;
    while y < y1 {
        let mut x = x0;
        while x < x1 {
            let px0 = ((x * out.sx).round() as i32).clamp(0, bw - 1) as usize;
            let py0 = ((y * out.sy).round() as i32).clamp(0, bh - 1) as usize;
            let px1 = (((x + block) * out.sx).round() as i32).clamp(px0 as i32 + 1, bw) as usize;
            let py1 = (((y + block) * out.sy).round() as i32).clamp(py0 as i32 + 1, bh) as usize;
            let (mut rs, mut gs, mut bs, mut n) = (0u64, 0u64, 0u64, 0u64);
            let mut py = py0;
            while py < py1 {
                let row = py * stride;
                let mut px = px0;
                while px < px1 {
                    let i = row + px * 4;
                    if i + 3 < data.len() {
                        bs += data[i] as u64;
                        gs += data[i + 1] as u64;
                        rs += data[i + 2] as u64;
                        n += 1;
                    }
                    px += step;
                }
                py += step;
            }
            if n > 0 {
                cr.set_source_rgb(
                    rs as f64 / n as f64 / 255.0,
                    gs as f64 / n as f64 / 255.0,
                    bs as f64 / n as f64 / 255.0,
                );
                cr.rectangle(
                    x,
                    y,
                    (x + block).min(x1) - x,
                    (y + block).min(y1) - y,
                );
                let _ = cr.fill();
            }
            x += block;
        }
        y += block;
    }
    let _ = cr.restore();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Doc, OutputLayer, Selection};

    /// 造一块统一亮灰的显示器画面：逻辑 lw*lh，缩放 sx
    fn doc_with(lw: i32, lh: i32, sx: f64) -> Doc {
        let pw = (lw as f64 * sx).round() as i32;
        let ph = (lh as f64 * sx).round() as i32;
        let mut phys = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph).unwrap();
        {
            let stride = phys.stride() as usize;
            let mut data = phys.data().unwrap();
            for y in 0..ph as usize {
                for x in 0..pw as usize {
                    let i = y * stride + x * 4;
                    data[i] = 200;
                    data[i + 1] = 200;
                    data[i + 2] = 200;
                    data[i + 3] = 255;
                }
            }
        }
        phys.mark_dirty();
        Doc::new(
            vec![OutputLayer {
                size: (lw as f64, lh as f64),
                sx,
                sy: sx,
                phys,
                windows: Vec::new(),
            }],
            "/tmp".into(),
        )
    }

    fn render(doc: &mut Doc, w: i32, h: i32) -> (Vec<u8>, usize) {
        let mut target = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        {
            let cr = cairo::Context::new(&target).unwrap();
            paint(&cr, doc, 0, (w as f64, h as f64));
            let _ = cr.show_page();
        }
        let mut data = vec![0u8; (w * h * 4) as usize];
        {
            let stride = target.stride() as usize;
            let src = target.data().unwrap();
            for y in 0..h as usize {
                let s = y * stride;
                data[y * w as usize * 4..(y + 1) * w as usize * 4]
                    .copy_from_slice(&src[s..s + w as usize * 4]);
            }
        }
        (data, w as usize)
    }

    fn gray(data: &[u8], w: usize, x: usize, y: usize) -> u8 {
        data[(y * w + x) * 4 + 1]
    }

    #[test]
    fn paints_dim_outside_and_keeps_selection_bright() {
        let mut doc = doc_with(400, 200, 2.0);
        doc.sel = Some(Selection {
            out: 0,
            x0: 100.0,
            y0: 50.0,
            x1: 200.0,
            y1: 100.0,
        });
        let (data, w) = render(&mut doc, 400, 200);
        // 选区内保持原亮度
        assert!(gray(&data, w, 150, 75) >= 195, "选区内 {}", gray(&data, w, 150, 75));
        assert!(gray(&data, w, 105, 55) >= 195);
        // 选区外被压暗（0.45 黑）
        assert!(gray(&data, w, 10, 10) <= 120, "选区外 {}", gray(&data, w, 10, 10));
        assert!(gray(&data, w, 380, 180) <= 120);
        // 选区正下方边界外也要压暗
        assert!(gray(&data, w, 150, 104) <= 120, "下侧 {}", gray(&data, w, 150, 104));
        assert!(gray(&data, w, 150, 44) <= 120, "上侧 {}", gray(&data, w, 150, 44));
    }

    #[test]
    fn paints_without_selection() {
        let mut doc = doc_with(400, 200, 2.0);
        let (data, w) = render(&mut doc, 400, 200);
        assert!(gray(&data, w, 0, 0) <= 120);
        assert!(gray(&data, w, 200, 100) <= 120);
    }

    #[test]
    fn paints_degenerate_selection_without_panic() {
        let mut doc = doc_with(400, 200, 2.0);
        for (x0, y0) in [(0.0, 0.0), (399.0, 199.0), (200.0, 100.0)] {
            doc.sel = Some(Selection {
                out: 0,
                x0,
                y0,
                x1: x0,
                y1: y0,
            });
            let _ = render(&mut doc, 400, 200);
        }
        // 反向选区（从右下往左上拖）
        doc.sel = Some(Selection {
            out: 0,
            x0: 300.0,
            y0: 150.0,
            x1: 100.0,
            y1: 50.0,
        });
        let (data, w) = render(&mut doc, 400, 200);
        assert!(gray(&data, w, 200, 100) >= 195, "反向选区内部 {}", gray(&data, w, 200, 100));
    }
}
