// 导出：选区渲染、PNG 编码、剪贴板、落盘
use std::path::{Path, PathBuf};

use gtk::cairo;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::doc::{intersect, Doc};
use crate::paint::draw_stroke;

/// 把抓屏得到的 BGRA 像素包成画布
pub fn surface_from_bgra(width: u32, height: u32, pixels: &[u8]) -> Result<cairo::ImageSurface, String> {
    let w = width as i32;
    let h = height as i32;
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)
        .map_err(|e| format!("创建画布失败: {e}"))?;
    {
        let stride = surface.stride() as usize;
        let row = width as usize * 4;
        let mut data = surface
            .data()
            .map_err(|e| format!("锁定画布失败: {e}"))?;
        if stride == row {
            let n = row * height as usize;
            data[..n].copy_from_slice(&pixels[..n]);
        } else {
            for y in 0..height as usize {
                let src = y * row;
                let dst = y * stride;
                data[dst..dst + row].copy_from_slice(&pixels[src..src + row]);
            }
        }
    }
    surface.mark_dirty();
    Ok(surface)
}

/// 抓屏的 BGRA 像素直接当纹理用，接管所有权
///
/// GDK 的显存格式本来就认 BGRA（cairo 画布也是这个顺序），不用再整屏换通道：
/// 5120x2160 上能省掉一次 44 MB 的搬运。抓屏出来的 alpha 都是 255，
/// 按预乘处理与直通一致。
pub fn texture_from_bgra_owned(pixels: Vec<u8>, w: i32, h: i32) -> gdk::Texture {
    let bytes = glib::Bytes::from_owned(pixels);
    gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        (w as usize) * 4,
    )
    .upcast()
}

/// 渲染选区：跨屏时把各屏画面在物理像素上拼成一张整图
pub fn render_selection(doc: &mut Doc) -> Result<cairo::ImageSurface, String> {
    let sel = doc.sel.ok_or("还没有选区")?.normalized();
    let Doc { outs, strokes, .. } = doc;

    // 逐屏算出跟选区的交集；参考缩放取最大的一档，避免把画面降采样
    let mut hits = Vec::new();
    let (mut kx, mut ky) = (0.0f64, 0.0f64);
    for (i, o) in outs.iter().enumerate() {
        if let Some(r) = intersect(sel, o.rect()) {
            hits.push((i, r));
            kx = kx.max(o.sx);
            ky = ky.max(o.sy);
        }
    }
    if hits.is_empty() {
        return Err("选区不在任何显示器上".into());
    }
    let lw = (sel.2 - sel.0).max(0.1);
    let lh = (sel.3 - sel.1).max(0.1);
    let pw = ((lw * kx).round().max(1.0)) as i32;
    let ph = ((lh * ky).round().max(1.0)) as i32;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph)
        .map_err(|e| format!("创建输出画布失败: {e}"))?;
    {
        let cr = cairo::Context::new(&surface).map_err(|e| format!("建立绘图上下文失败: {e}"))?;

        // 底图：每块屏把自己那一段贴到画布上对应的位置
        for (i, r) in &hits {
            let o = &outs[*i];
            let px0 = ((r.0 - o.origin.0) * o.sx).round();
            let py0 = ((r.1 - o.origin.1) * o.sy).round();
            let _ = cr.save();
            cr.translate((r.0 - sel.0) * kx, (r.1 - sel.1) * ky);
            cr.scale(kx / o.sx, ky / o.sy);
            cr.translate(-px0, -py0);
            let pat = cairo::SurfacePattern::create(&o.phys);
            pat.set_filter(cairo::Filter::Nearest);
            if cr.set_source(&pat).is_ok() {
                let _ = cr.paint();
            }
            let _ = cr.restore();
        }

        // 标注：全局逻辑坐标映射到同一张画布
        let _ = cr.save();
        cr.scale(kx, ky);
        cr.translate(-sel.0, -sel.1);
        cr.rectangle(sel.0, sel.1, sel.2 - sel.0, sel.3 - sel.1);
        cr.clip();
        for s in strokes.iter() {
            draw_stroke(&cr, outs, s);
        }
        let _ = cr.restore();
        let _ = cr.show_page();
    }
    surface.flush();
    Ok(surface)
}

/// 画布转 RGBA 像素
///
/// 通道顺序要换一下（cairo 的 ARGB32 在内存里是 BGRX）。按 4 字节一组整块处理，
/// 比逐像素按下标读写快得多，整屏 1100 万像素也就十几毫秒。
pub fn surface_rgba(surface: &mut cairo::ImageSurface) -> Result<(Vec<u8>, i32, i32), String> {
    let w = surface.width() as usize;
    let h = surface.height() as usize;
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| format!("读取画布失败: {e}"))?;
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let srow = &data[y * stride..y * stride + w * 4];
        let drow = &mut out[y * w * 4..(y + 1) * w * 4];
        for (s, d) in srow.chunks_exact(4).zip(drow.chunks_exact_mut(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    }
    Ok((out, w as i32, h as i32))
}

/// 从 RGBA 像素编码 PNG，不碰任何 GTK 对象，可以丢到别的线程上跑
pub fn encode_png_rgba(rgba: &[u8], w: i32, h: i32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("编码 PNG 失败: {e}"))?;
    Ok(out)
}

/// 编码 PNG
pub fn encode_png(surface: &mut cairo::ImageSurface) -> Result<Vec<u8>, String> {
    let (rgba, w, h) = surface_rgba(surface)?;
    encode_png_rgba(&rgba, w, h)
}

/// RGBA 像素转剪贴板纹理
pub fn texture_from_rgba(rgba: &[u8], w: i32, h: i32) -> Result<gdk::Texture, String> {
    Ok(texture_from_rgba_owned(rgba.to_vec(), w, h))
}

/// 同上，但接管这块像素的所有权，省掉一次整屏拷贝
pub fn texture_from_rgba_owned(rgba: Vec<u8>, w: i32, h: i32) -> gdk::Texture {
    let bytes = glib::Bytes::from_owned(rgba);
    gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        (w as usize) * 4,
    )
    .upcast()
}

fn stamp() -> String {
    glib::DateTime::now_local()
        .and_then(|t| t.format("%Y-%m-%d_%H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "screenshot".to_string())
}

fn unique_path(dir: &Path, base: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}.png"));
    let mut i = 2;
    while p.exists() {
        p = dir.join(format!("{base}-{i}.png"));
        i += 1;
    }
    p
}

/// 保存到目录，返回落盘路径
pub fn save_to_dir(surface: &mut cairo::ImageSurface, dir: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    let path = unique_path(Path::new(dir), &stamp());
    let bytes = encode_png(surface)?;
    std::fs::write(&path, bytes).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(path)
}

/// 把 RGBA 像素编码后落到图片目录，返回落盘路径
///
/// 和 save_to_dir 等价，只是不依赖 cairo 画布，可以拿到后台线程上跑。
pub fn save_rgba_png(rgba: &[u8], w: i32, h: i32, dir: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    let path = unique_path(Path::new(dir), &stamp());
    let bytes = encode_png_rgba(rgba, w, h)?;
    std::fs::write(&path, bytes).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(path)
}

/// 保存到指定路径
pub fn save_to_path(surface: &mut cairo::ImageSurface, path: &str) -> Result<PathBuf, String> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    let bytes = encode_png(surface)?;
    std::fs::write(path, bytes).map_err(|e| format!("写入文件失败: {e}"))?;
    Ok(PathBuf::from(path))
}

/// 把 PNG 交给 wl-copy 持有，不依赖键盘焦点
pub fn copy_png_via_wl_copy(png: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut child = std::process::Command::new("wl-copy")
        .args(["--type", "image/png"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("无法启动 wl-copy: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(png)
            .map_err(|e| format!("写入 wl-copy 失败: {e}"))?;
    }
    match child.wait() {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err(format!("wl-copy 退出码 {}", st.code().unwrap_or(-1))),
        Err(e) => Err(format!("等待 wl-copy 失败: {e}")),
    }
}

/// 桌面通知，失败就算了
pub fn notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .arg("-a")
        .arg("FocusSnapS")
        .arg(summary)
        .arg(body)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::OutputLayer;

    /// 造一块 (逻辑宽, 逻辑高, 缩放) 的显示器画面，R 通道编码 x、G 通道编码 y
    fn layer_at(origin: (f64, f64), lw: i32, lh: i32, sx: f64, sy: f64) -> OutputLayer {
        let pw = (lw as f64 * sx).round() as i32;
        let ph = (lh as f64 * sy).round() as i32;
        let mut phys = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph).unwrap();
        {
            let stride = phys.stride() as usize;
            let mut data = phys.data().unwrap();
            for y in 0..ph as usize {
                for x in 0..pw as usize {
                    let i = y * stride + x * 4;
                    data[i] = 0;
                    data[i + 1] = y as u8;
                    data[i + 2] = x as u8;
                    data[i + 3] = 255;
                }
            }
        }
        phys.mark_dirty();
        OutputLayer::new(
            origin,
            (lw as f64, lh as f64),
            sx,
            sy,
            phys,
            Vec::new(),
        )
    }

    fn layer(lw: i32, lh: i32, sx: f64, sy: f64) -> OutputLayer {
        layer_at((0.0, 0.0), lw, lh, sx, sy)
    }

    /// 直接按选区渲染
    fn render(doc: &mut Doc, sel: (f64, f64, f64, f64)) -> cairo::ImageSurface {
        doc.sel = Some(crate::doc::Selection {
            x0: sel.0,
            y0: sel.1,
            x1: sel.2,
            y1: sel.3,
        });
        render_selection(doc).unwrap()
    }

    #[test]
    fn render_rect_maps_physical_pixels_exactly() {
        // 逻辑 40x20，2 倍缩放 → 物理 80x40
        let mut doc = Doc::new(vec![layer(40, 20, 2.0, 2.0)], "/tmp".into());
        let mut out = render(&mut doc, (10.0, 5.0, 20.0, 10.0));
        // 选区 10x5 逻辑 → 20x10 物理
        assert_eq!((out.width(), out.height()), (20, 10));
        let (rgba, w, _) = surface_rgba(&mut out).unwrap();
        // 左上角对应物理 (20,10)
        let px = |x: usize, y: usize, c: usize| rgba[(y * w as usize + x) * 4 + c] as i32;
        assert!((px(0, 0, 0) - 20).abs() <= 1, "R={}", px(0, 0, 0));
        assert!((px(0, 0, 1) - 10).abs() <= 1, "G={}", px(0, 0, 1));
        assert!((px(19, 9, 0) - 39).abs() <= 1, "R={}", px(19, 9, 0));
        assert!((px(19, 9, 1) - 19).abs() <= 1, "G={}", px(19, 9, 1));
        // 导出的图必须不透明
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn render_rect_handles_fractional_scale() {
        // 逻辑 100x50，1.4 倍 → 物理 140x70
        let mut doc = Doc::new(vec![layer(100, 50, 1.4, 1.4)], "/tmp".into());
        let mut out = render(&mut doc, (10.0, 10.0, 60.0, 40.0));
        assert_eq!((out.width(), out.height()), (70, 42));
        let (rgba, w, _) = surface_rgba(&mut out).unwrap();
        let px = |x: usize, y: usize, c: usize| rgba[(y * w as usize + x) * 4 + c] as f64;
        // 物理起点 10*1.4=14，+0.5 采样偏移
        assert!((px(0, 0, 0) - 14.5).abs() <= 2.0, "R={}", px(0, 0, 0));
        assert!((px(0, 0, 1) - 14.5).abs() <= 2.0, "G={}", px(0, 0, 1));
    }

    #[test]
    fn render_selection_stitches_outputs() {
        // 两块 40x20 逻辑、2 倍缩放的屏并排，横跨两屏选一条带
        let mut doc = Doc::new(
            vec![
                layer_at((0.0, 0.0), 40, 20, 2.0, 2.0),
                layer_at((40.0, 0.0), 40, 20, 2.0, 2.0),
            ],
            "/tmp".into(),
        );
        let mut out = render(&mut doc, (30.0, 5.0, 50.0, 15.0));
        // 20 逻辑宽 → 40 物理宽，10 逻辑高 → 20 物理高
        assert_eq!((out.width(), out.height()), (40, 20));
        let (rgba, w, _) = surface_rgba(&mut out).unwrap();
        let px = |x: usize, y: usize, c: usize| rgba[(y * w as usize + x) * 4 + c] as i32;
        // 左半段来自第一块屏：物理 x 从 60 起，y 从 10 起
        assert!((px(0, 0, 0) - 60).abs() <= 1, "左半 R={}", px(0, 0, 0));
        assert!((px(0, 0, 1) - 10).abs() <= 1, "左半 G={}", px(0, 0, 1));
        // 右半段来自第二块屏，它的本地 x 从 0 重新开始
        assert!((px(20, 0, 0) - 0).abs() <= 1, "右半 R={}", px(20, 0, 0));
        assert!((px(39, 19, 0) - 19).abs() <= 1, "右半末 R={}", px(39, 19, 0));
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn unique_path_avoids_overwrite() {
        let dir = std::env::temp_dir().join(format!("ks-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "shot");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "shot");
        assert_ne!(first, second);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
