// 导出：选区渲染、PNG 编码、剪贴板、落盘
use std::path::{Path, PathBuf};

use gtk::cairo;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::doc::Doc;
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

/// 渲染一段区域：逻辑坐标 (x0,y0,x1,y1) → 按物理分辨率输出
fn render_rect(
    doc: &mut Doc,
    out_idx: usize,
    r: (f64, f64, f64, f64),
) -> Result<cairo::ImageSurface, String> {
    let Doc { outs, strokes, .. } = doc;
    let Some(out) = outs.get_mut(out_idx) else {
        return Err("选区所属的显示器已失效".into());
    };
    // 夹到显示器范围内，并对齐到物理像素边界，避免整图重采样
    let x0 = r.0.max(0.0);
    let y0 = r.1.max(0.0);
    let x1 = r.2.min(out.size.0).max(x0 + 1.0);
    let y1 = r.3.min(out.size.1).max(y0 + 1.0);
    let px0 = (x0 * out.sx).round();
    let py0 = (y0 * out.sy).round();
    let pw = ((x1 * out.sx).round() - px0).max(1.0) as i32;
    let ph = ((y1 * out.sy).round() - py0).max(1.0) as i32;
    let lw = (x1 - x0).max(0.1);
    let lh = (y1 - y0).max(0.1);
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph)
        .map_err(|e| format!("创建输出画布失败: {e}"))?;
    {
        let cr = cairo::Context::new(&surface).map_err(|e| format!("建立绘图上下文失败: {e}"))?;
        // 底图：物理像素 1:1 贴到画布左上角
        let _ = cr.save();
        cr.translate(-px0, -py0);
        let pat = cairo::SurfacePattern::create(&out.phys);
        pat.set_filter(cairo::Filter::Nearest);
        if cr.set_source(&pat).is_ok() {
            let _ = cr.paint();
        }
        let _ = cr.restore();
        // 标注：逻辑坐标映射到同一张画布
        let _ = cr.save();
        cr.scale(pw as f64 / lw, ph as f64 / lh);
        cr.translate(-x0, -y0);
        for s in strokes.iter() {
            draw_stroke(&cr, out, s);
        }
        let _ = cr.restore();
        let _ = cr.show_page();
    }
    surface.flush();
    Ok(surface)
}

/// 渲染当前选区
pub fn render_selection(doc: &mut Doc) -> Result<cairo::ImageSurface, String> {
    let sel = doc.sel.ok_or("还没有选区")?;
    render_rect(doc, sel.out, sel.normalized())
}

/// 画布转 RGBA8 字节
fn surface_rgba(surface: &mut cairo::ImageSurface) -> Result<(Vec<u8>, i32, i32), String> {
    let w = surface.width() as usize;
    let h = surface.height() as usize;
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| format!("读取画布失败: {e}"))?;
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = y * stride;
        let dst = y * w * 4;
        let srow = &data[src..src + w * 4];
        let drow = &mut out[dst..dst + w * 4];
        for x in 0..w {
            let i = x * 4;
            drow[i] = srow[i + 2];
            drow[i + 1] = srow[i + 1];
            drow[i + 2] = srow[i];
            drow[i + 3] = srow[i + 3];
        }
    }
    Ok((out, w as i32, h as i32))
}

/// 编码 PNG
pub fn encode_png(surface: &mut cairo::ImageSurface) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let (rgba, w, h) = surface_rgba(surface)?;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("编码 PNG 失败: {e}"))?;
    Ok(out)
}

/// 画布转剪贴板纹理
pub fn surface_texture(surface: &mut cairo::ImageSurface) -> Result<gdk::Texture, String> {
    let (rgba, w, h) = surface_rgba(surface)?;
    let bytes = glib::Bytes::from(&rgba);
    Ok(gdk::MemoryTexture::new(
        w,
        h,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        (w as usize) * 4,
    )
    .upcast())
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
    fn layer(lw: i32, lh: i32, sx: f64, sy: f64) -> OutputLayer {
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
        OutputLayer {
            size: (lw as f64, lh as f64),
            sx,
            sy,
            phys,
            windows: Vec::new(),
        }
    }

    #[test]
    fn render_rect_maps_physical_pixels_exactly() {
        // 逻辑 40x20，2 倍缩放 → 物理 80x40
        let mut doc = Doc::new(vec![layer(40, 20, 2.0, 2.0)], "/tmp".into());
        let mut out = render_rect(&mut doc, 0, (10.0, 5.0, 20.0, 10.0)).unwrap();
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
        let mut out = render_rect(&mut doc, 0, (10.0, 10.0, 60.0, 40.0)).unwrap();
        assert_eq!((out.width(), out.height()), (70, 42));
        let (rgba, w, _) = surface_rgba(&mut out).unwrap();
        let px = |x: usize, y: usize, c: usize| rgba[(y * w as usize + x) * 4 + c] as f64;
        // 物理起点 10*1.4=14，+0.5 采样偏移
        assert!((px(0, 0, 0) - 14.5).abs() <= 2.0, "R={}", px(0, 0, 0));
        assert!((px(0, 0, 1) - 14.5).abs() <= 2.0, "G={}", px(0, 0, 1));
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
