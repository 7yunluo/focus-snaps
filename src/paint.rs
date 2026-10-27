// 浮层绘制：画面、遮罩、标注、选区边框与手柄
use gtk::cairo;
use gtk::pango;
use pangocairo::functions::{create_layout, show_layout};

use crate::doc::{intersect, Doc, OutputLayer, Rect4, Stroke, Tool};

/// 选区外压暗的强度
const DIM: f64 = 0.45;
/// 手柄边长（逻辑像素）
pub const HANDLE: f64 = 7.0;
/// 手柄命中半径（逻辑像素）
pub const HANDLE_HIT: f64 = 9.0;

/// 画一个显示器的整块浮层，area 是窗口的实际逻辑尺寸
///
/// 选区、标注都存在全局逻辑坐标里，各屏把自己跟选区求交集的那一段画出来，
/// 多画的落在 surface 外被合成器裁掉，视觉上就拼成一块连续的浮层。
pub fn paint(
    cr: &cairo::Context,
    doc: &mut Doc,
    out_idx: usize,
    area: (f64, f64),
    preview: Option<(f64, f64, &str)>,
) {
    // 本屏的参数先拷出来，后面要可变借用文档
    let (origin, size, sx, sy, phys, out_rect) = match doc.outs.get(out_idx) {
        Some(o) => (o.origin, o.size, o.sx, o.sy, o.phys.clone(), o.rect()),
        None => return,
    };
    let sel = doc.sel.map(|s| s.normalized());
    let hover = doc.hover.as_ref().map(|r| (r.x0, r.y0, r.x1, r.y1));
    // 尺寸徽标只在选区左上角所在的那块屏上画
    let owner = sel.map(|s| doc.out_at(s.0, s.1)) == Some(Some(out_idx));

    // 画面按物理分辨率 1:1 贴到设备像素，避免二次采样发虚
    let _ = cr.save();
    cr.scale(1.0 / sx, 1.0 / sy);
    let _ = cr.set_source_surface(&phys, 0.0, 0.0);
    let _ = cr.paint();
    let _ = cr.restore();

    let Doc {
        outs,
        strokes,
        active,
        tool,
        width,
        accent,
        text_sel,
        ..
    } = doc;

    // 选区（或悬停窗口）落在本屏上的那一段，就是遮罩要挖的洞
    let hole = sel
        .and_then(|s| intersect(s, out_rect))
        .or_else(|| hover.and_then(|h| intersect(h, out_rect)));

    // 选区外（或悬停窗口外）压暗；这一步还在本屏局部坐标里画
    cr.set_source_rgba(0.0, 0.0, 0.0, DIM);
    match hole {
        Some((x0, y0, x1, y1)) => {
            let (x0, y0) = (x0 - origin.0, y0 - origin.1);
            let (x1, y1) = (x1 - origin.0, y1 - origin.1);
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

    // 之后统一按全局逻辑坐标画，只是把原点挪到本屏的左上角
    let _ = cr.save();
    cr.translate(-origin.0, -origin.1);

    let Some((x0, y0, x1, y1)) = sel else {
        // 还没框选时，把指针悬停的窗口勾出来
        if let Some(h) = hover.filter(|h| intersect(*h, out_rect).is_some()) {
            draw_hover(cr, h, *accent);
        }
        let _ = cr.restore();
        return;
    };

    // 标注只画在选区内
    let _ = cr.save();
    cr.rectangle(x0, y0, x1 - x0, y1 - y0);
    cr.clip();
    for s in strokes.iter() {
        draw_stroke(cr, outs, s);
    }
    if let Some(a) = active.as_ref() {
        draw_stroke(cr, outs, a);
    }
    // 正在输入的文字由输入框原地显示，这里只框出它的范围
    if let Some((px, py, text)) = preview {
        if !text.is_empty() {
            let layout = text_layout(cr, text, text_px(*width), true);
            let (tw, th) = layout.pixel_size();
            draw_sel_frame(cr, (px, py, px + f64::from(tw), py + f64::from(th)), *accent);
        }
    }
    let _ = cr.restore();

    draw_frame(cr, (x0, y0, x1, y1), *tool == Tool::Select, *accent);
    draw_text_frame(cr, strokes, *text_sel, *accent);
    let _ = cr.restore();

    if owner {
        // 徽标按本屏局部坐标摆，才不会算错贴边
        let local = (x0 - origin.0, y0 - origin.1, x1 - origin.0, y1 - origin.1);
        draw_badge(cr, local, size, (sx, sy));
    }
}

fn quad(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64) {
    if w > 0.0 && h > 0.0 {
        cr.rectangle(x, y, w, h);
    }
}

/// 悬停窗口的高亮框（全局逻辑坐标）
fn draw_hover(cr: &cairo::Context, r: Rect4, accent: (f64, f64, f64)) {
    let (x0, y0, x1, y1) = r;
    let _ = cr.save();
    cr.set_source_rgba(accent.0, accent.1, accent.2, 0.95);
    cr.set_line_width(2.0);
    cr.rectangle(x0, y0, x1 - x0, y1 - y0);
    let _ = cr.stroke();
    let _ = cr.restore();
}

/// 被选中 / 正在输入的文字外面的虚线框
fn draw_text_frame(
    cr: &cairo::Context,
    strokes: &[Stroke],
    text_sel: Option<usize>,
    accent: (f64, f64, f64),
) {
    let Some(s) = text_sel.and_then(|i| strokes.get(i)) else {
        return;
    };
    let (Some((w, h)), Some(&(x, y))) = (s.text_size, s.points.first()) else {
        return;
    };
    draw_sel_frame(cr, (x, y, x + w, y + h), accent);
}

fn draw_sel_frame(cr: &cairo::Context, r: (f64, f64, f64, f64), accent: (f64, f64, f64)) {
    let (x0, y0, x1, y1) = r;
    let _ = cr.save();
    cr.set_source_rgba(accent.0, accent.1, accent.2, 0.9);
    cr.set_line_width(1.0);
    cr.set_dash(&[4.0, 3.0], 0.0);
    cr.rectangle(x0 - 3.0, y0 - 3.0, x1 - x0 + 6.0, y1 - y0 + 6.0);
    let _ = cr.stroke();
    let _ = cr.restore();
}

/// 选区边框 + 拖拽手柄
fn draw_frame(
    cr: &cairo::Context,
    r: (f64, f64, f64, f64),
    handles: bool,
    accent: (f64, f64, f64),
) {
    let (x0, y0, x1, y1) = r;
    let _ = cr.save();
    cr.set_line_width(1.0);
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.55);
    cr.rectangle(x0 - 0.5, y0 - 0.5, x1 - x0 + 1.0, y1 - y0 + 1.0);
    let _ = cr.stroke();

    cr.set_source_rgba(accent.0, accent.1, accent.2, 1.0);
    cr.rectangle(x0 + 0.5, y0 + 0.5, x1 - x0 - 1.0, y1 - y0 - 1.0);
    let _ = cr.stroke();

    if handles {
        cr.set_source_rgba(accent.0, accent.1, accent.2, 1.0);
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
fn draw_badge(
    cr: &cairo::Context,
    r: Rect4,
    size: (f64, f64),
    scale: (f64, f64),
) {
    let (x0, y0, x1, y1) = r;
    let wpx = ((x1 - x0) * scale.0).round() as i64;
    let hpx = ((y1 - y0) * scale.1).round() as i64;
    let label = format!("{wpx} × {hpx}");
    let layout = text_layout(cr, &label, 13.0, false);
    let (tw, th) = layout.pixel_size();
    let pad = 6.0;
    let bw = tw as f64 + pad * 2.0;
    let bh = th as f64 + 6.0;
    // 优先放在选区上方，其次下方，都放不下就压进选区内部并保证不出界
    let bx = x0.clamp(2.0, (size.0 - bw - 2.0).max(2.0));
    let by = if y0 - bh - 6.0 >= 2.0 {
        y0 - bh - 6.0
    } else if y1 + 6.0 + bh <= size.1 - 2.0 {
        y1 + 6.0
    } else {
        (y0 + 6.0).clamp(2.0, (size.1 - bh - 2.0).max(2.0))
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
    let mut desc = pango::FontDescription::new();
    desc.set_family(&system_font_family());
    if bold {
        desc.set_weight(pango::Weight::Bold);
    }
    desc.set_absolute_size(px * f64::from(pango::SCALE));
    layout.set_font_description(Some(&desc));
    layout.set_text(text);
    layout
}

/// 系统界面字体的家族名，取不到就用 Sans
pub fn system_font_family() -> String {
    if !gtk::is_initialized() {
        return "Sans".into();
    }
    gtk::Settings::default()
        .and_then(|s| s.gtk_font_name())
        .map(|n| n.to_string())
        .map(|n| match n.rsplit_once(' ') {
            // "Adwaita Sans 11" 末尾是字号，去掉它只留家族名
            Some((family, size)) if size.chars().all(|c| c.is_ascii_digit()) => family.to_string(),
            _ => n,
        })
        .unwrap_or_else(|| "Sans".into())
}

/// 文字工具的字号：跟着线宽走
pub fn text_px(width: f64) -> f64 {
    (width * 5.0).max(13.0)
}

/// 量一段文字的排版尺寸（逻辑像素）
pub fn text_size(surface: &cairo::ImageSurface, text: &str, px: f64) -> Option<(f64, f64)> {
    let cr = cairo::Context::new(surface).ok()?;
    let layout = text_layout(&cr, text, px, true);
    let (w, h) = layout.pixel_size();
    Some((f64::from(w), f64::from(h)))
}

/// 画一笔标注（全局逻辑坐标）
pub fn draw_stroke(cr: &cairo::Context, outs: &mut [OutputLayer], s: &Stroke) {
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
                Tool::Mosaic => paint_mosaic(cr, outs, (x0, y0), (x1, y1)),
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
                let layout = text_layout(cr, text, text_px(s.width), true);
                let _ = cr.save();
                cr.set_source_rgba(r, g, b, 1.0);
                cr.move_to(s.points[0].0, s.points[0].1);
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
///
/// 打码块跨越屏边界时，按屏切开分别取各自画面的像素。
fn paint_mosaic(cr: &cairo::Context, outs: &mut [OutputLayer], a: (f64, f64), b: (f64, f64)) {
    let want = (
        a.0.min(b.0),
        a.1.min(b.1),
        a.0.max(b.0),
        a.1.max(b.1),
    );
    for out in outs.iter_mut() {
        if let Some(r) = intersect(want, out.rect()) {
            mosaic_piece(cr, out, r);
        }
    }
}

/// 在一块屏的范围内打码，r 是全局逻辑坐标
fn mosaic_piece(cr: &cairo::Context, out: &mut OutputLayer, r: Rect4) {
    let stride = out.phys.stride() as usize;
    let (bw, bh) = (out.phys.width(), out.phys.height());
    let (x0, y0, x1, y1) = r;
    let data = match out.phys.data() {
        Ok(d) => d,
        Err(_) => return,
    };
    // 块大小随缩放微调，观感上始终是 12 逻辑像素
    let block = 12.0;
    let step = 3usize;
    // 块网格对齐到全局坐标，跨屏时纹路才是连续的
    let gx0 = (x0 / block).floor() * block;
    let gy0 = (y0 / block).floor() * block;

    let _ = cr.save();
    cr.rectangle(x0, y0, x1 - x0, y1 - y0);
    cr.clip();
    let mut y = gy0;
    while y < y1 {
        let mut x = gx0;
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
            vec![OutputLayer::new(
                (0.0, 0.0),
                (lw as f64, lh as f64),
                sx,
                sx,
                phys,
                Vec::new(),
            )],
            "/tmp".into(),
        )
    }

    fn render(doc: &mut Doc, w: i32, h: i32) -> (Vec<u8>, usize) {
        render_out(doc, 0, w, h)
    }

    fn render_out(doc: &mut Doc, idx: usize, w: i32, h: i32) -> (Vec<u8>, usize) {
        let mut target = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        {
            let cr = cairo::Context::new(&target).unwrap();
            paint(&cr, doc, idx, (w as f64, h as f64), None);
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
    fn paints_selection_slice_on_each_output() {
        // 两块 200x100 逻辑、1 倍缩放的屏并排，选区从第一块横跨到第二块
        let mut doc = doc_with(200, 100, 1.0);
        let mut phys = cairo::ImageSurface::create(cairo::Format::ARgb32, 200, 100).unwrap();
        {
            let stride = phys.stride() as usize;
            let mut data = phys.data().unwrap();
            for y in 0..100usize {
                for x in 0..200usize {
                    let i = y * stride + x * 4;
                    data[i] = 200;
                    data[i + 1] = 200;
                    data[i + 2] = 200;
                    data[i + 3] = 255;
                }
            }
        }
        phys.mark_dirty();
        doc.outs.push(OutputLayer::new(
            (200.0, 0.0),
            (200.0, 100.0),
            1.0,
            1.0,
            phys,
            Vec::new(),
        ));
        doc.sel = Some(Selection {
            x0: 150.0,
            y0: 20.0,
            x1: 250.0,
            y1: 60.0,
        });
        // 第一块屏：本屏 x 150..200 那一段保持原亮度，其余压暗
        let (data, w) = render_out(&mut doc, 0, 200, 100);
        assert!(gray(&data, w, 160, 40) >= 195, "左屏选区内 {}", gray(&data, w, 160, 40));
        assert!(gray(&data, w, 100, 40) <= 120, "左屏选区外 {}", gray(&data, w, 100, 40));
        assert!(gray(&data, w, 110, 90) <= 120, "左屏选区下方 {}", gray(&data, w, 110, 90));
        // 第二块屏：本屏 x 0..50 那一段是亮的，右边压暗
        let (data, w) = render_out(&mut doc, 1, 200, 100);
        assert!(gray(&data, w, 25, 40) >= 195, "右屏选区内 {}", gray(&data, w, 25, 40));
        assert!(gray(&data, w, 100, 40) <= 120, "右屏选区外 {}", gray(&data, w, 100, 40));
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
                x0,
                y0,
                x1: x0,
                y1: y0,
            });
            let _ = render(&mut doc, 400, 200);
        }
        // 反向选区（从右下往左上拖）
        doc.sel = Some(Selection {
            x0: 300.0,
            y0: 150.0,
            x1: 100.0,
            y1: 50.0,
        });
        let (data, w) = render(&mut doc, 400, 200);
        assert!(gray(&data, w, 200, 100) >= 195, "反向选区内部 {}", gray(&data, w, 200, 100));
    }
}



