// 窗口识别：从抓到的画面里找平铺/浮动窗口的边界
//
// niri 不对外提供平铺窗口的像素坐标（tile_pos_in_workspace_view 只有浮动窗口才有），
// 所以这里直接分析画面：窗口竖边贯穿整个窗口高度，内容边缘只有很短的竖向延续，
// 按「整列对比度」打分就能把窗口左右边界挑出来，一对边界就是一个窗口。
use gtk::cairo;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

/// 相邻窗口之间允许的间隙上限（逻辑像素）
const GAP_MAX: f64 = 40.0;
/// 一个窗口至少多宽才认（逻辑像素）
const MIN_W: f64 = 80.0;
/// 一个窗口至少多高才认（逻辑像素）
const MIN_H: f64 = 80.0;
/// 单点对比度阈值（0-255 的亮度差）
const EDGE_MIN: f32 = 18.0;

/// 先用 niri IPC 的列宽当先验，再在画面里解出布局偏移；拿不到就先纯图像识别
pub fn detect_for_output(
    connector: &str,
    surface: &mut cairo::ImageSurface,
    sx: f64,
    sy: f64,
    area: (f64, f64),
) -> Vec<Rect> {
    if let Some(wins) = crate::niri::active_workspace_windows(connector) {
        // 全屏窗口：niri 仍然给 pos_in_scrolling_layout，但 tile_size 会等于整个输出
        // （平铺窗口横竖都被间隙和状态栏缩进，能分开）
        let gaps = crate::niri::gaps();
        let mut fullscreen: Vec<Rect> = Vec::new();
        for w in wins.iter() {
            let fill_w = w.tile_size.0 >= area.0 - gaps;
            let fill_h = w.tile_size.1 >= area.1 - gaps;
            if (!w.tiled && !w.floating) || (!w.floating && fill_w && fill_h) {
                let size = if w.size.0 > 1.0 && w.size.1 > 1.0 {
                    w.size
                } else {
                    w.tile_size
                };
                let x0 = w.offset.0.max(0.0);
                let y0 = w.offset.1.max(0.0);
                fullscreen.push(Rect {
                    x0,
                    y0,
                    x1: (x0 + size.0).min(area.0),
                    y1: (y0 + size.1).min(area.1),
                });
            }
        }
        if !fullscreen.is_empty() {
            return fullscreen;
        }

        // 同一列的多个窗口是 tab，只算一列
        // (列序号, 窗口宽, 列宽, 横向偏移, 窗口高)
        let mut cols: Vec<(u32, f64, f64, f64, f64)> = Vec::new();
        let mut floating: Vec<Rect> = Vec::new();
        for w in wins.iter() {
            if w.floating {
                if let Some((x, y)) = w.tile_pos {
                    floating.push(Rect {
                        x0: x,
                        y0: y,
                        x1: x + w.size.0,
                        y1: y + w.size.1,
                    });
                }
                continue;
            }
            if let Some(slot) = cols.iter_mut().find(|c| c.0 == w.col) {
                slot.1 = slot.1.max(w.size.0);
                slot.2 = slot.2.max(w.tile.0);
                slot.3 = slot.3.max(w.offset.0);
                slot.4 = slot.4.max(w.size.1);
            } else {
                cols.push((w.col, w.size.0, w.tile.0, w.offset.0, w.size.1));
            }
        }
        cols.sort_by_key(|c| c.0);
        if !cols.is_empty() {
            let plan: Vec<(f64, f64, f64, f64)> = cols.iter().map(|c| (c.1, c.2, c.3, c.4)).collect();
            if let Some(mut rects) = solve_offset(surface, sx, sy, area, &plan) {
                rects.extend(floating);
                rects.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
                return rects;
            }
        } else {
            // IPC 说这块屏的工作区没有平铺窗口，就只有浮动窗口（可能为空）
            return floating;
        }
    }
    detect(surface, sx, sy, area)
}

/// 给命令行离线验证用
pub fn solve_offset_pub(
    surface: &mut cairo::ImageSurface,
    sx: f64,
    sy: f64,
    area: (f64, f64),
    cols: &[(f64, f64)],
) -> Option<Vec<Rect>> {
    let plan: Vec<(f64, f64, f64, f64)> = cols.iter().map(|c| (c.0, c.1, 0.0, 0.0)).collect();
    solve_offset(surface, sx, sy, area, &plan)
}

/// 已知每列的宽度，只解一个偏移量：让所有列的左右边界都落在画面里的强竖边上
fn solve_offset(
    surface: &mut cairo::ImageSurface,
    sx: f64,
    sy: f64,
    area: (f64, f64),
    cols: &[(f64, f64, f64, f64)],
) -> Option<Vec<Rect>> {
    let w = surface.width() as usize;
    let h = surface.height() as usize;
    let stride = surface.stride() as usize;
    if w < 64 || h < 64 {
        return None;
    }
    let gap0 = crate::niri::gaps();
    let (score, max_score) = {
        let data = surface.data().ok()?;
        let mut score = vec![0f32; w];
        for (x, slot) in score.iter_mut().enumerate().take(w - 1) {
            let mut acc = 0f32;
            for y in 0..h {
                acc += (lum(&data, stride, x, y) - lum(&data, stride, x + 1, y)).abs();
            }
            *slot = acc;
        }
        let mx = score.iter().cloned().fold(0f32, f32::max);
        (score, mx)
    };
    if max_score <= 0.0 {
        return None;
    }

    let mut best: Option<(f64, f64, f32)> = None;
    for gap in [gap0 - 2.0, gap0, gap0 + 2.0, gap0 + 4.0] {
        if gap < 0.0 {
            continue;
        }
        let total: f64 = cols.iter().map(|c| c.1 + gap).sum();
        let _ = total;
        let mut t = -total;
        while t <= area.0 {
            let mut s = 0f32;
            let mut x = t;
            for c in cols.iter() {
                let (b0, b1) = (x + c.2, x + c.2 + c.0);
                for b in [b0, b1] {
                    if b >= 0.0 && b < area.0 {
                        let px = ((b * sx).round() as usize).min(w - 2);
                        s += score[px];
                    }
                }
                x += c.1 + gap;
            }
            if best.map(|(_, _, bs)| s > bs).unwrap_or(true) {
                best = Some((t, gap, s));
            }
            t += 1.0;
        }
    }
    let (t, gap, best_score) = best?;
    // 对不上画面就认输，交给纯图像识别
    if best_score < max_score * 0.5 {
        return None;
    }

    let (top, bottom) = {
        let data = surface.data().ok()?;
        horizontal_bounds(&data, stride, w, h, sy).unwrap_or((0.0, area.1))
    };
    let mut rects: Vec<Rect> = Vec::new();
    let mut x = t;
    for c in cols.iter() {
        let (x0, x1) = ((x + c.2).max(0.0), (x + c.2 + c.0).min(area.0));
        // 高度以 IPC 为准（没给就用画面量到的），只借画面的顶边
        let y0 = top;
        let y1 = if c.3 > 0.0 {
            (top + c.3).min(area.1)
        } else {
            bottom
        };
        if x1 - x0 >= MIN_W && y1 - y0 >= MIN_H {
            rects.push(Rect {
                x0,
                y0,
                x1,
                y1,
            });
        }
        x += c.1 + gap;
    }
    if rects.is_empty() {
        None
    } else {
        Some(rects)
    }
}

/// 从一张画面里找出窗口矩形，返回逻辑坐标
pub fn detect(
    surface: &mut cairo::ImageSurface,
    sx: f64,
    sy: f64,
    area: (f64, f64),
) -> Vec<Rect> {
    let w = surface.width() as usize;
    let h = surface.height() as usize;
    let stride = surface.stride() as usize;
    if w < 64 || h < 64 {
        return Vec::new();
    }
    let data = match surface.data() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    // 逐列纵向对比度：窗口竖边贯穿整窗高度，内容边缘只有很短的竖向延续
    // 松阈值用来补回被阴影糊掉的上下边缘
    let soft = EDGE_MIN * 0.3;
    let mut score = vec![0f32; w];
    let mut run = vec![(0usize, 0usize); w];
    for x in 0..w - 1 {
        let mut acc = 0f32;
        let (mut best_start, mut best_len) = (0usize, 0usize);
        let (mut cur_start, mut cur_len) = (0usize, 0usize);
        let (mut soft_start, mut soft_len) = (0usize, 0usize);
        let (mut sc, mut sl) = (0usize, 0usize);
        for y in 0..h {
            let d = (lum(&data, stride, x, y) - lum(&data, stride, x + 1, y)).abs();
            acc += d;
            if d >= EDGE_MIN {
                if cur_len == 0 {
                    cur_start = y;
                }
                cur_len += 1;
                if cur_len > best_len {
                    best_len = cur_len;
                    best_start = cur_start;
                }
            } else {
                cur_len = 0;
            }
            if d >= soft {
                if sl == 0 {
                    sc = y;
                }
                sl += 1;
                if sl > soft_len {
                    soft_len = sl;
                    soft_start = sc;
                }
            } else {
                sl = 0;
            }
        }
        score[x] = acc;
        // 松阈值范围最多比严阈值多出一个间隙宽度
        let max_extra = (GAP_MAX * 3.0) as usize;
        let lo = soft_start.max(best_start.saturating_sub(max_extra));
        let hi = (soft_start + soft_len).min(best_start + best_len + max_extra);
        run[x] = (lo, hi.saturating_sub(lo).max(best_len));
    }

    let max_score = score.iter().cloned().fold(0f32, f32::max);
    if max_score <= 0.0 {
        return Vec::new();
    }
    let thr = max_score * 0.12;

    // 合并相邻列，得到候选竖边（位置 + 竖向延续范围）
    let mut edges: Vec<(usize, (usize, usize))> = Vec::new();
    let mut x = 0usize;
    while x < w - 1 {
        if score[x] >= thr {
            let mut best = x;
            while x < w - 1 && score[x] >= thr {
                if score[x] > score[best] {
                    best = x;
                }
                x += 1;
            }
            // 太矮的边更像窗口内部的花纹，不是窗框
            if run[best].1 * 8 >= h {
                edges.push((best, run[best]));
            }
        } else {
            x += 1;
        }
    }
    if edges.len() < 2 {
        return Vec::new();
    }

    // 一个间隙里往往有好几条边（两侧窗框 + 阴影线），先把挨得近的合成一条边界带
    let cluster_px = 24.0 * sx;
    let mut bounds: Vec<(usize, usize)> = Vec::new(); // (边界带左端, 右端)
    for (x, _run) in edges.iter() {
        match bounds.last_mut() {
            Some(last) if (*x - last.1) as f64 <= cluster_px => last.1 = *x,
            _ => bounds.push((*x, *x)),
        }
    }
    if bounds.is_empty() {
        return Vec::new();
    }
    // 屏幕边缘可能没有检测到窗框边（贴边的窗口外侧没有间隙），补成虚拟边界
    if (bounds[0].0 as f64) > cluster_px {
        bounds.insert(0, (0, 0));
    }
    if ((w - bounds[bounds.len() - 1].1) as f64) > cluster_px {
        bounds.push((w, w));
    }

    if std::env::var("FOCUSSNAPS_DEBUG_EDGES").is_ok() {
        eprintln!("thr={thr:.0} max={max_score:.0} edges={}", edges.len());
        for (x, r) in edges.iter() {
            eprintln!(
                "  edge x={x} ({:.1} 逻辑) score={:.0} ({:.2}max) run={}",
                *x as f64 / sx,
                score[*x],
                score[*x] / max_score,
                r.1
            );
        }
        for (l, r) in bounds.iter() {
            eprintln!("  band {l}..{r} ({:.1}..{:.1} 逻辑)", *l as f64 / sx, *r as f64 / sx);
        }
    }
    let mut out: Vec<Rect> = Vec::new();
    let push = |out: &mut Vec<Rect>, lx: usize, ly: (usize, usize), rx: usize, ry: (usize, usize)| {
        let y0 = ly.0.min(ry.0) as f64 / sy;
        let y1 = (ly.0 + ly.1).max(ry.0 + ry.1) as f64 / sy;
        let r = Rect {
            x0: lx as f64 / sx,
            y0,
            x1: rx as f64 / sx,
            y1,
        };
        if r.width() >= MIN_W
            && r.height() >= MIN_H
            && r.x0 >= -1.0
            && r.x1 <= area.0 + 1.0
            && r.y1 <= area.1 + 1.0
        {
            out.push(r);
        }
    };

    let full = (0usize, h);
    for i in 0..bounds.len().saturating_sub(1) {
        let l = bounds[i].1;
        let r = bounds[i + 1].0;
        let ly = edges.iter().find(|e| e.0 == l).map(|e| e.1).unwrap_or(full);
        let ry = edges.iter().find(|e| e.0 == r).map(|e| e.1).unwrap_or(full);
        push(&mut out, l, ly, r, ry);
    }

    // 圆角会让竖边在上下两端提前消失，这里用横向边把上下边界钉准。
    // 自己上下都有明显横向边缘的（浮动窗口）保留原范围，其余按平铺窗口的统一边界来。
    if let Some((y0, y1)) = horizontal_bounds(&data, stride, w, h, sy) {
        let py0 = y0 * sy;
        let py1 = y1 * sy;
        for r in out.iter_mut() {
            let x0 = (r.x0 * sx).max(0.0) as usize;
            let x1 = ((r.x1 * sx) as usize).min(w - 2);
            let own_top = has_h_edge(&data, stride, x0, x1, py0 as usize);
            let own_bottom = has_h_edge(&data, stride, x0, x1, py1 as usize);
            if !(own_top && own_bottom) {
                r.y0 = y0;
                r.y1 = y1;
            }
        }
    }
    out
}

/// 某一行在给定横向范围内是否有明显的横向边缘
fn has_h_edge(data: &[u8], stride: usize, x0: usize, x1: usize, y: usize) -> bool {
    if x1 <= x0 {
        return false;
    }
    let mut acc = 0f32;
    let mut n = 0f32;
    let mut x = x0;
    while x < x1 {
        acc += (lum(data, stride, x, y) - lum(data, stride, x, y + 1)).abs();
        n += 1.0;
        x += 4;
    }
    n > 0.0 && acc / n > 10.0
}

/// 所有平铺窗口共用的上下边界：找最强的两条横向长边
fn horizontal_bounds(
    data: &[u8],
    stride: usize,
    w: usize,
    h: usize,
    sy: f64,
) -> Option<(f64, f64)> {
    let mut row = vec![0f32; h];
    for (y, slot) in row.iter_mut().enumerate().take(h - 1) {
        let mut acc = 0f32;
        let mut x = 0usize;
        while x < w - 1 {
            acc += (lum(data, stride, x, y) - lum(data, stride, x, y + 1)).abs();
            x += 2;
        }
        *slot = acc;
    }
    let max = row.iter().cloned().fold(0f32, f32::max);
    if max <= 0.0 {
        return None;
    }
    let thr = max * 0.25;
    // 候选：过门限的行的局部极值
    let mut peaks: Vec<(usize, f32)> = Vec::new();
    for y in 1..h - 1 {
        if row[y] >= thr && row[y] >= row[y - 1] && row[y] >= row[y + 1] {
            peaks.push((y, row[y]));
        }
    }
    if peaks.len() < 2 {
        return None;
    }
    // 取相距超过半屏、总强度最高的一对
    let mut best: Option<(usize, usize, f32)> = None;
    for i in 0..peaks.len() {
        for j in i + 1..peaks.len() {
            let d = peaks[j].0 - peaks[i].0;
            if (d as f64) < h as f64 * 0.5 {
                continue;
            }
            let s = peaks[i].1 + peaks[j].1;
            if best.map(|(_, _, bs)| s > bs).unwrap_or(true) {
                best = Some((peaks[i].0, peaks[j].0, s));
            }
        }
    }
    let (a, b, _) = best?;
    Some(((a + 1) as f64 / sy, b as f64 / sy))
}

#[inline]
fn lum(data: &[u8], stride: usize, x: usize, y: usize) -> f32 {
    let i = y * stride + x * 4;
    if i + 3 >= data.len() {
        return 0.0;
    }
    // BGRA 内存序
    0.114 * data[i] as f32 + 0.587 * data[i + 1] as f32 + 0.299 * data[i + 2] as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张「左右两个窗口 + 中间间隙」的图
    fn two_windows(w: i32, h: i32) -> cairo::ImageSurface {
        let mut s = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).unwrap();
        {
            let stride = s.stride() as usize;
            let mut d = s.data().unwrap();
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let i = y * stride + x * 4;
                    // 背景灰 40；窗口内亮 200，但不含边界处
                    let in_win = (x >= 14 && x < 100) || (x >= 114 && x < 200);
                    let v = if in_win { 200u8 } else { 40u8 };
                    // 窗口内部有点内容边缘，但只占一小段高度
                    let noise = if in_win && (x % 7 == 0) && (60..100).contains(&y) { 60 } else { 0 };
                    d[i] = v.saturating_sub(noise);
                    d[i + 1] = v.saturating_sub(noise);
                    d[i + 2] = v.saturating_sub(noise);
                    d[i + 3] = 255;
                }
            }
        }
        s.mark_dirty();
        s
    }

    #[test]
    fn detects_two_side_by_side_windows() {
        let s = two_windows(214, 240);
        let mut s = s;
        let rects = detect(&mut s, 1.0, 1.0, (214.0, 240.0));
        assert_eq!(rects.len(), 2, "应识别出两个窗口: {rects:?}");
        assert!((rects[0].x0 - 14.0).abs() <= 2.0, "{:?}", rects[0]);
        assert!((rects[0].x1 - 100.0).abs() <= 2.0, "{:?}", rects[0]);
        assert!((rects[1].x0 - 114.0).abs() <= 2.0, "{:?}", rects[1]);
        assert!((rects[1].x1 - 200.0).abs() <= 2.0, "{:?}", rects[1]);
        // 高度应覆盖整块可视区
        assert!(rects[0].height() >= 200.0, "{:?}", rects[0]);
    }

    /// 造一张多列不等宽的画面：列宽 + 间隙 + 统一的上下边
    fn columns_image(widths: &[i32], gap: i32, h: i32) -> cairo::ImageSurface {
        let total: i32 = widths.iter().sum::<i32>() + gap * (widths.len() as i32 + 1);
        let mut s = cairo::ImageSurface::create(cairo::Format::ARgb32, total, h).unwrap();
        {
            let stride = s.stride() as usize;
            let mut d = s.data().unwrap();
            for y in 0..h as usize {
                for x in 0..total as usize {
                    d[y * stride + x * 4 + 3] = 255;
                }
            }
            // 背景 30，窗口内 180，上下各留 gap
            let mut x = gap;
            for (i, w) in widths.iter().enumerate() {
                for y in (gap as usize)..(h as usize - gap as usize) {
                    for xx in (x as usize)..((x + w) as usize) {
                        let v = if i % 2 == 0 { 180u8 } else { 150u8 };
                        let i4 = y * stride + xx * 4;
                        d[i4] = v;
                        d[i4 + 1] = v;
                        d[i4 + 2] = v;
                    }
                }
                x += w + gap;
            }
        }
        s.mark_dirty();
        s
    }

    #[test]
    fn solve_offset_recovers_mixed_widths() {
        let widths = [600, 420, 500];
        let gap = 10;
        let mut img = columns_image(&widths, gap, 300);
        let total = 600 + 420 + 500 + gap * 4;
        let plan: Vec<(f64, f64, f64, f64)> = widths
            .iter()
            .map(|w| (*w as f64, *w as f64, 0.0, 300.0 - 2.0 * gap as f64))
            .collect();
        let rects = solve_offset(&mut img, 1.0, 1.0, (total as f64, 300.0), &plan)
            .expect("应该能解出偏移");
        assert!(rects.len() >= 2, "{rects:?}");
        let mut want_x = gap as f64;
        for (i, w) in widths.iter().enumerate() {
            let r = rects.iter().find(|r| (r.x0 - want_x).abs() < 4.0);
            assert!(r.is_some(), "第 {i} 列应该在 x={want_x} 附近: {rects:?}");
            let r = r.unwrap();
            assert!((r.width() - *w as f64).abs() < 4.0, "{r:?}");
            want_x += *w as f64 + gap as f64;
        }
    }

    #[test]
    fn empty_image_yields_nothing() {
        let mut s = cairo::ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
        {
            let stride = s.stride() as usize;
            let mut d = s.data().unwrap();
            d[..stride * 64].fill(90);
        }
        s.mark_dirty();
        assert!(detect(&mut s, 1.0, 1.0, (64.0, 64.0)).is_empty());
    }
}
