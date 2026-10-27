// 浮层绘制：画面、遮罩、标注、选区边框与手柄
use gtk::cairo;
use gtk::pango;
use pangocairo::functions::{create_layout, show_layout};

use crate::doc::{intersect, Doc, OutputLayer, Rect4, Stroke, Tool};

/// 选区外压暗的强度
pub const DIM: f64 = 0.45;
/// 手柄边长（逻辑像素）
pub const HANDLE: f64 = 7.0;
/// 手柄命中半径（逻辑像素）
pub const HANDLE_HIT: f64 = 9.0;

/// 标注层的余量（逻辑像素）：手柄、边框伸出去的部分，加上选区上方那块尺寸徽标
pub const ANNOT_PAD: f64 = 34.0;

/// 标注层需要覆盖的范围（本屏局部坐标）
///
/// 只画会画出东西的那一块：选区（或悬停窗口）加余量，再裁进本屏。
/// 返回 None 表示这屏上什么都不用画，图层可以整个藏起来。
pub fn annot_rect(live: Option<Rect4>, out_rect: Rect4) -> Option<Rect4> {
    let r = intersect(live?, out_rect)?;
    let x0 = (r.0 - ANNOT_PAD).max(out_rect.0);
    let y0 = (r.1 - ANNOT_PAD).max(out_rect.1);
    let x1 = (r.2 + ANNOT_PAD).min(out_rect.2);
    let y1 = (r.3 + ANNOT_PAD).min(out_rect.3);
    let local = (
        x0 - out_rect.0,
        y0 - out_rect.1,
        x1 - out_rect.0,
        y1 - out_rect.1,
    );
    if local.2 > local.0 && local.3 > local.1 {
        Some(local)
    } else {
        None
    }
}

/// 压暗要挖的洞：选区（或悬停窗口）跟本屏的交集，换算成本屏局部坐标
///
/// 跨屏选区时每块屏挖自己这一段的洞，拼起来就是连续的一块。
pub fn hole_rect(sel: Option<Rect4>, hover: Option<Rect4>, out_rect: Rect4) -> Option<Rect4> {
    let g = sel
        .and_then(|s| intersect(s, out_rect))
        .or_else(|| hover.and_then(|h| intersect(h, out_rect)))?;
    Some((
        g.0 - out_rect.0,
        g.1 - out_rect.1,
        g.2 - out_rect.0,
        g.3 - out_rect.1,
    ))
}

/// 压暗用的矩形：洞以外的部分切成上、下、左、右四块（本屏局部坐标）
///
/// 没有洞就是整屏一块。这些矩形交给 CSS 画，合成器填实心色块不花钱，
/// 比每帧让 cairo 调一遍 blend 便宜得多。
pub fn dim_rects(hole: Option<Rect4>, area: (f64, f64)) -> Vec<Rect4> {
    let (w, h) = area;
    let Some((x0, y0, x1, y1)) = hole else {
        return if w > 0.0 && h > 0.0 {
            vec![(0.0, 0.0, w, h)]
        } else {
            Vec::new()
        };
    };
    let mut out = Vec::with_capacity(4);
    for r in [
        (0.0, 0.0, w, y0),
        (0.0, y1, w, h),
        (0.0, y0, x0, y1),
        (x1, y0, w, y1),
    ] {
        if r.2 > r.0 && r.3 > r.1 {
            out.push(r);
        }
    }
    out
}

/// 画一个显示器上随交互变化的那一层：悬停框、标注、选区边框
///
/// 屏幕画面在下层由 GtkPicture 合成、压暗在上层由 CSS 矩形铺，这里都不管。
///
/// 选区、标注都存在全局逻辑坐标里。`view` 是这张画布左上角的全局坐标：
/// 画布只覆盖会画出东西的那块地方（选区加上一点余量），不用铺满整屏。
pub fn paint(
    cr: &cairo::Context,
    doc: &mut Doc,
    out_idx: usize,
    view: (f64, f64),
    preview: Option<(f64, f64, &str)>,
) {
    // 本屏的参数先拷出来，后面要可变借用文档
    let (origin, size, sx, sy, out_rect) = match doc.outs.get(out_idx) {
        Some(o) => (o.origin, o.size, o.sx, o.sy, o.rect()),
        None => return,
    };
    let sel = doc.sel.map(|s| s.normalized());
    let hover = doc.hover.as_ref().map(|r| (r.x0, r.y0, r.x1, r.y1));
    // 尺寸徽标只在选区左上角所在的那块屏上画
    let owner = sel.map(|s| doc.out_at(s.0, s.1)) == Some(Some(out_idx));

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

    // 之后统一按全局逻辑坐标画，只是把原点挪到画布左上角
    let _ = cr.save();
    cr.translate(-view.0, -view.1);

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
        // 徽标按本屏局部坐标摆才不会算错贴边，所以先切回屏幕坐标再画
        let _ = cr.save();
        cr.translate(origin.0 - view.0, origin.1 - view.1);
        draw_badge(cr, (x0 - origin.0, y0 - origin.1, x1 - origin.0, y1 - origin.1), size, (sx, sy));
        let _ = cr.restore();
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

/// 打码块的大小（逻辑像素）
const MOSAIC_BLOCK: f64 = 12.0;

/// 区域打码：从预计算好的块平均图上贴一块
///
/// 块平均图是整屏一次性算出来的（mosaic_image），每帧只是把它按块贴在选区里，
/// 不再每帧去扫原画面的像素。
fn paint_mosaic(cr: &cairo::Context, outs: &mut [OutputLayer], a: (f64, f64), b: (f64, f64)) {
    let want = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
    for out in outs.iter_mut() {
        if let Some(r) = intersect(want, out.rect()) {
            mosaic_piece(cr, out, r);
        }
    }
}

/// 在一部分屏幕上贴打码块，r 是全局逻辑坐标
fn mosaic_piece(cr: &cairo::Context, out: &mut OutputLayer, r: Rect4) {
    let origin = mosaic_origin(out);
    let Some(img) = mosaic_image(out) else {
        return;
    };
    let pat = cairo::SurfacePattern::create(&img);
    // 一块一个纯色，必须按最近邻放大，不然块边缘会糊
    pat.set_filter(cairo::Filter::Nearest);
    let _ = cr.save();
    cr.rectangle(r.0, r.1, r.2 - r.0, r.3 - r.1);
    cr.clip();
    cr.translate(origin.0, origin.1);
    cr.scale(MOSAIC_BLOCK, MOSAIC_BLOCK);
    let _ = cr.set_source(&pat);
    let _ = cr.paint();
    let _ = cr.restore();
}

/// 块平均图的左上角（全局逻辑坐标），对齐到全局的块网格
///
/// 网格按全局坐标对齐，跨屏打码时两边的纹路才是接得上的。
pub fn mosaic_origin(out: &OutputLayer) -> (f64, f64) {
    (
        (out.origin.0 / MOSAIC_BLOCK).floor() * MOSAIC_BLOCK,
        (out.origin.1 / MOSAIC_BLOCK).floor() * MOSAIC_BLOCK,
    )
}

/// 取（必要时先算）本屏的块平均图
///
/// 每个像素代表 12x12 逻辑像素的平均色，sRGB 直通不进线性空间，贴上去跟原来的
/// 逐块取平均等价。
pub fn mosaic_image(out: &mut OutputLayer) -> Option<cairo::ImageSurface> {
    if out.mosaic.is_none() {
        out.mosaic = build_mosaic(out);
    }
    out.mosaic.clone()
}

fn build_mosaic(out: &mut OutputLayer) -> Option<cairo::ImageSurface> {
    let (gx0, gy0) = mosaic_origin(out);
    let (w, h) = out.size;
    let cols = ((w + (out.origin.0 - gx0)) / MOSAIC_BLOCK).ceil() as i32;
    let rows = ((h + (out.origin.1 - gy0)) / MOSAIC_BLOCK).ceil() as i32;
    if cols <= 0 || rows <= 0 {
        return None;
    }
    let mut img = cairo::ImageSurface::create(cairo::Format::ARgb32, cols, rows).ok()?;
    let (origin, sx, sy) = (out.origin, out.sx, out.sy);
    let phys = out.ensure_phys()?;
    let stride = phys.stride() as usize;
    let (bw, bh) = (phys.width(), phys.height());
    {
        let data = phys.data().ok()?;
        let dst_stride = img.stride() as usize;
        let mut dst = img.data().ok()?;
        for j in 0..rows {
            for i in 0..cols {
                // 这一块覆盖的物理像素范围
                let lx0 = gx0 + f64::from(i) * MOSAIC_BLOCK;
                let ly0 = gy0 + f64::from(j) * MOSAIC_BLOCK;
                let px0 = (((lx0 - origin.0) * sx).round().max(0.0) as i32).clamp(0, bw - 1);
                let py0 = (((ly0 - origin.1) * sy).round().max(0.0) as i32).clamp(0, bh - 1);
                let px1 = (((lx0 + MOSAIC_BLOCK - origin.0) * sx).round() as i32)
                    .clamp(px0 + 1, bw) as usize;
                let py1 = (((ly0 + MOSAIC_BLOCK - origin.1) * sy).round() as i32)
                    .clamp(py0 + 1, bh) as usize;
                let (mut rs, mut gs, mut bs, mut n) = (0u64, 0u64, 0u64, 0u64);
                for py in py0 as usize..py1 {
                    let row = py * stride;
                    for px in px0 as usize..px1 {
                        let k = row + px * 4;
                        bs += data[k] as u64;
                        gs += data[k + 1] as u64;
                        rs += data[k + 2] as u64;
                        n += 1;
                    }
                }
                let o = (j as usize) * dst_stride + (i as usize) * 4;
                if n > 0 {
                    dst[o] = (bs / n) as u8;
                    dst[o + 1] = (gs / n) as u8;
                    dst[o + 2] = (rs / n) as u8;
                }
                dst[o + 3] = 255;
            }
        }
    }
    img.mark_dirty();
    Some(img)
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
            // 下层画面如今由 GtkPicture 合成，这里铺一层同样的浅灰当作它
            cr.set_source_rgb(200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0);
            let _ = cr.paint();
            let view = doc.outs[idx].origin;
            paint(&cr, doc, idx, view, None);
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

    /// 把遮罩矩形叠到浅灰底上，模拟 CSS 实心矩形压在画面之上
    fn render_dim(hole: Option<Rect4>, w: i32, h: i32) -> (Vec<u8>, usize) {
        let mut target = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        {
            let cr = cairo::Context::new(&target).unwrap();
            cr.set_source_rgb(200.0 / 255.0, 200.0 / 255.0, 200.0 / 255.0);
            let _ = cr.paint();
            cr.set_source_rgba(0.0, 0.0, 0.0, DIM);
            for (x0, y0, x1, y1) in dim_rects(hole, (f64::from(w), f64::from(h))) {
                cr.rectangle(x0, y0, x1 - x0, y1 - y0);
            }
            let _ = cr.fill();
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

    #[test]
    fn dim_keeps_hole_bright_and_darkens_outside() {
        let out_rect = (0.0, 0.0, 400.0, 200.0);
        let hole = hole_rect(Some((100.0, 50.0, 200.0, 100.0)), None, out_rect);
        let (data, w) = render_dim(hole, 400, 200);
        // 洞里保持原亮度
        assert!(gray(&data, w, 150, 75) >= 195, "洞里 {}", gray(&data, w, 150, 75));
        assert!(gray(&data, w, 105, 55) >= 195);
        // 洞外被压暗（0.45 黑）
        assert!(gray(&data, w, 10, 10) <= 120, "洞外 {}", gray(&data, w, 10, 10));
        assert!(gray(&data, w, 380, 180) <= 120);
        // 洞的上下边界外也要压暗
        assert!(gray(&data, w, 150, 104) <= 120, "下侧 {}", gray(&data, w, 150, 104));
        assert!(gray(&data, w, 150, 44) <= 120, "上侧 {}", gray(&data, w, 150, 44));
    }

    #[test]
    fn dim_hole_follows_each_output() {
        // 两块 200x100 逻辑的屏并排，选区从第一块横跨到第二块
        let sel = (150.0, 20.0, 250.0, 60.0);
        let left = (0.0, 0.0, 200.0, 100.0);
        let right = (200.0, 0.0, 400.0, 100.0);
        // 第一块屏：本屏 x 150..200 那一段是洞，其余压暗
        let (data, w) = render_dim(hole_rect(Some(sel), None, left), 200, 100);
        assert!(gray(&data, w, 160, 40) >= 195, "左屏洞里 {}", gray(&data, w, 160, 40));
        assert!(gray(&data, w, 100, 40) <= 120, "左屏洞外 {}", gray(&data, w, 100, 40));
        assert!(gray(&data, w, 110, 90) <= 120, "左屏洞下方 {}", gray(&data, w, 110, 90));
        // 第二块屏：本屏 x 0..50 那一段是洞，右边压暗
        let (data, w) = render_dim(hole_rect(Some(sel), None, right), 200, 100);
        assert!(gray(&data, w, 25, 40) >= 195, "右屏洞里 {}", gray(&data, w, 25, 40));
        assert!(gray(&data, w, 100, 40) <= 120, "右屏洞外 {}", gray(&data, w, 100, 40));
    }

    #[test]
    fn dim_covers_whole_screen_without_hole() {
        let (data, w) = render_dim(None, 400, 200);
        assert!(gray(&data, w, 0, 0) <= 120);
        assert!(gray(&data, w, 200, 100) <= 120);
        // 还没框选时，悬停的窗口也能当洞
        let out_rect = (0.0, 0.0, 400.0, 200.0);
        let hover = (10.0, 10.0, 60.0, 60.0);
        assert_eq!(hole_rect(None, Some(hover), out_rect), Some(hover));
        // 洞铺满整屏时不该产生任何矩形
        assert!(dim_rects(Some(out_rect), (400.0, 200.0)).is_empty());
    }

    #[test]
    fn degenerate_selection_leaves_no_hole() {
        let out_rect = (0.0, 0.0, 400.0, 200.0);
        for (x0, y0) in [(0.0, 0.0), (399.0, 199.0), (200.0, 100.0)] {
            assert_eq!(
                hole_rect(Some((x0, y0, x0, y0)), None, out_rect),
                None,
                "零面积选区不该挖洞"
            );
        }
        // 反向选区先归一化，照样挖洞
        assert_eq!(
            hole_rect(Some((100.0, 50.0, 300.0, 150.0)), None, out_rect),
            Some((100.0, 50.0, 300.0, 150.0))
        );
        // 选区完全在别的屏上时，本屏整块压暗
        assert_eq!(hole_rect(Some((500.0, 50.0, 600.0, 80.0)), None, out_rect), None);
        assert_eq!(dim_rects(None, (400.0, 200.0)), vec![(0.0, 0.0, 400.0, 200.0)]);
    }

    #[test]
    fn annot_rect_pads_live_area_and_clamps_to_screen() {
        let out_rect = (0.0, 0.0, 1000.0, 800.0);
        let pad = ANNOT_PAD;
        // 屏幕中间的选区：四周各留一点余量
        assert_eq!(
            annot_rect(Some((100.0, 100.0, 300.0, 200.0)), out_rect),
            Some((100.0 - pad, 100.0 - pad, 300.0 + pad, 200.0 + pad))
        );
        // 贴边的选区裁在本屏内
        assert_eq!(
            annot_rect(Some((0.0, 0.0, 300.0, 200.0)), out_rect),
            Some((0.0, 0.0, 300.0 + pad, 200.0 + pad))
        );
        // 跨屏选区：本屏只取自己那一段
        assert_eq!(
            annot_rect(Some((900.0, 100.0, 1400.0, 200.0)), out_rect),
            Some((900.0 - pad, 100.0 - pad, 1000.0, 200.0 + pad))
        );
        // 没有选区也没有悬停窗口，整层可以藏起来
        assert_eq!(annot_rect(None, out_rect), None);
        // 选区完全在别的屏上
        assert_eq!(
            annot_rect(Some((2000.0, 0.0, 2100.0, 100.0)), out_rect),
            None
        );
        // 全屏选区是退化的最坏情况，仍然是整屏
        assert_eq!(annot_rect(Some(out_rect), out_rect), Some(out_rect));
    }

    /// 造一块左黑右白的画面：分界在逻辑坐标 split 处
    fn doc_half(w: i32, h: i32, split: f64) -> Doc {
        let mut phys = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        {
            let cr = cairo::Context::new(&phys).unwrap();
            cr.set_source_rgb(0.0, 0.0, 0.0);
            let _ = cr.paint();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.rectangle(split, 0.0, f64::from(w) - split, f64::from(h));
            let _ = cr.fill();
        }
        phys.mark_dirty();
        Doc::new(
            vec![OutputLayer::new(
                (0.0, 0.0),
                (f64::from(w), f64::from(h)),
                1.0,
                1.0,
                phys,
                Vec::new(),
            )],
            "/tmp".into(),
        )
    }

    #[test]
    fn mosaic_paints_block_averages() {
        // 240 宽的画面，黑白分界在 x=120（正好是第 10 个块）
        let mut doc = doc_half(240, 120, 120.0);
        doc.sel = Some(Selection {
            x0: 0.0,
            y0: 0.0,
            x1: 240.0,
            y1: 120.0,
        });
        doc.commit(Stroke {
            tool: Tool::Mosaic,
            color: (1.0, 0.0, 0.0),
            width: 4.0,
            points: vec![(0.0, 0.0), (240.0, 120.0)],
            text: None,
            number: None,
            text_size: None,
        });
        let (data, w) = render_out(&mut doc, 0, 240, 120);
        // 分界左边整块黑、右边整块白（避开左上角的尺寸徽标与边框手柄）
        assert!(gray(&data, w, 13, 50) <= 8, "左块 {}", gray(&data, w, 13, 50));
        assert!(gray(&data, w, 77, 50) <= 8, "左块 {}", gray(&data, w, 77, 50));
        assert!(gray(&data, w, 200, 50) >= 247, "右块 {}", gray(&data, w, 200, 50));
        // 同一个块里必须是一整片纯色，不能把原画面的像素露出来
        assert_eq!(gray(&data, w, 13, 50), gray(&data, w, 23, 58), "同块内应同色");
        assert_eq!(gray(&data, w, 194, 50), gray(&data, w, 205, 55), "同块内应同色");
    }

    #[test]
    fn mosaic_grid_aligns_to_global_coordinates() {
        // 屏从 x=1000 开始，块网格仍然对齐到全局的 12 的倍数，
        // 这样跨屏打码时两边的纹路接得上
        let mut doc = doc_half(240, 120, 120.0);
        doc.outs[0].origin = (1000.0, 0.0);
        let (gx0, gy0) = mosaic_origin(&doc.outs[0]);
        assert_eq!(gx0 % 12.0, 0.0);
        assert_eq!(gy0, 0.0);
        assert_eq!(gx0, 996.0, "1000 向上取整到 12 的倍数");
    }

    #[test]
    fn draws_annotation_inside_selection() {
        let mut doc = doc_with(400, 200, 2.0);
        doc.sel = Some(Selection {
            x0: 50.0,
            y0: 40.0,
            x1: 250.0,
            y1: 160.0,
        });
        doc.commit(Stroke {
            tool: Tool::Rect,
            color: (1.0, 0.0, 0.0),
            width: 3.0,
            points: vec![(80.0, 60.0), (200.0, 140.0)],
            text: None,
            number: None,
            text_size: None,
        });
        let (data, w) = render(&mut doc, 400, 200);
        // 矩形上边中点的像素（通道顺序 BGRA）应当是红的
        let i = (60 * w + 140) * 4;
        let px = &data[i..i + 3];
        assert!(px[2] > 150 && px[0] < 120, "标注没画上 {px:?}");
        // 标注层是透明的，选区外保留底色（压暗由 CSS 那层负责）
        assert!(gray(&data, w, 10, 10) >= 195);
    }
}
