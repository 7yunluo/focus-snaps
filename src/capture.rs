// 抓屏：枚举 Wayland 显示器并按合成器旋转归一化，取一帧像素
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsFd, OwnedFd};

use rustix::fs::{ftruncate, memfd_create, MemfdFlags};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self as frame, ZwlrScreencopyFrameV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use crate::dmabuf;

/// 一个显示器的 Wayland 侧信息
#[derive(Clone, Debug)]
pub struct OutputMeta {
    pub name: String,
    pub transform: wl_output::Transform,
    /// wl_output.scale，整数
    pub scale: i32,
    pub pos: (i32, i32),
    pub mode: (i32, i32),
}

impl Default for OutputMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            transform: wl_output::Transform::Normal,
            scale: 1,
            pos: (0, 0),
            mode: (0, 0),
        }
    }
}

/// 一帧画面，BGRA8888，行宽 = width * 4，已旋转到屏幕观感方向
///
/// 走零拷贝时 `pixels` 是空的，像素在 `dmabuf` 那块显存里，需要时 mmap 读。
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub dmabuf: Option<dmabuf::Buffer>,
}

impl Capture {
    /// 取出紧密排列的 BGRA 像素
    ///
    /// 零拷贝那条路画面在显存里，这里现读一份（CLI 的 grab/full 用得到）。
    pub fn into_pixels(self) -> Result<Vec<u8>, String> {
        if !self.pixels.is_empty() {
            return Ok(self.pixels);
        }
        match self.dmabuf {
            Some(buf) => buf.reader().read_pixels(),
            None => Err("抓屏没有返回画面".into()),
        }
    }
}

struct OutputSlot {
    wl: wl_output::WlOutput,
    meta: OutputMeta,
    have_name: bool,
    done: bool,
}

#[derive(Default)]
struct State {
    shm: Option<wl_shm::WlShm>,
    manager: Option<ZwlrScreencopyManagerV1>,
    outputs: Vec<OutputSlot>,
    geom: Option<(i32, i32, i32)>,
    fd: Option<OwnedFd>,
    _buffer: Option<wl_buffer::WlBuffer>,
    result: Option<Vec<u8>>,
    failed: Option<String>,
    finished: bool,
    /// 已经发过 copy，后续 buffer 事件忽略
    copied: bool,
    /// 像素是 RGB 字节序，需要交换 R/B
    swap_rb: bool,
    /// 合成器有没有 zwp_linux_dmabuf_v1
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    /// 它给这一帧报的显存格式与尺寸
    dmabuf_offer: Option<(u32, u32, u32)>,
    /// 这一次要不要走零拷贝
    use_dmabuf: bool,
    /// 零拷贝用的显存缓冲
    dmabuf_buf: Option<dmabuf::Buffer>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(slot) = state.outputs.iter_mut().find(|o| o.wl == *proxy) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => {
                slot.meta.name = name;
                slot.have_name = true;
            }
            wl_output::Event::Scale { factor } => slot.meta.scale = factor,
            wl_output::Event::Geometry { x, y, transform, .. } => {
                slot.meta.pos = (x, y);
                if let WEnum::Value(t) = transform {
                    slot.meta.transform = t;
                }
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                // 只记当前正在使用的模式
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                    slot.meta.mode = (width, height);
                }
            }
            wl_output::Event::Done => slot.done = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwlrScreencopyFrameV1,
        event: frame::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            frame::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                if state.copied || state.use_dmabuf {
                    return;
                }
                let format = match format {
                    WEnum::Value(f) => f,
                    WEnum::Unknown(_) => {
                        state.failed = Some("未知的 shm 像素格式".into());
                        state.finished = true;
                        return;
                    }
                };
                match format {
                    wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888 => state.swap_rb = false,
                    wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888 => state.swap_rb = true,
                    other => {
                        state.failed = Some(format!("不支持的像素格式 {other:?}"));
                        state.finished = true;
                        return;
                    }
                }
                let size = stride as usize * height as usize;
                let fd = match memfd_create("FocusSnapS", MemfdFlags::CLOEXEC) {
                    Ok(fd) => fd,
                    Err(e) => {
                        state.failed = Some(format!("memfd 创建失败: {e}"));
                        state.finished = true;
                        return;
                    }
                };
                if let Err(e) = ftruncate(&fd, size as u64) {
                    state.failed = Some(format!("分配共享内存失败: {e}"));
                    state.finished = true;
                    return;
                }
                let Some(shm) = state.shm.as_ref() else {
                    state.failed = Some("合成器未提供 wl_shm".into());
                    state.finished = true;
                    return;
                };
                let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
                let (wi, hi, si) = (width as i32, height as i32, stride as i32);
                let buffer = pool.create_buffer(0, wi, hi, si, format, qh, ());
                proxy.copy(&buffer);
                state.copied = true;
                state.geom = Some((wi, hi, si));
                state.fd = Some(fd);
                state._buffer = Some(buffer);
            }
            frame::Event::LinuxDmabuf {
                format,
                width,
                height,
            } => {
                state.dmabuf_offer = Some((format, width, height));
            }
            frame::Event::BufferDone => {
                if state.copied || !state.use_dmabuf {
                    return;
                }
                let (Some(dmabuf), Some((format, w, h))) =
                    (state.dmabuf.clone(), state.dmabuf_offer)
                else {
                    return;
                };
                match dmabuf::Buffer::allocate(w, h, format) {
                    Ok(buf) => {
                        let wl = buf.wrap(&dmabuf, qh);
                        proxy.copy(&wl);
                        state.copied = true;
                        state.dmabuf_buf = Some(buf);
                        state._buffer = Some(wl);
                    }
                    Err(e) => {
                        // 分配不出来就退回共享内存那条路（frame 已经用掉，下次再说）
                        eprintln!("零拷贝抓屏不可用（{e}），退回共享内存");
                        state.failed = Some(e);
                        state.finished = true;
                    }
                }
            }
            frame::Event::Ready { .. } => {
                state.finished = true;
                // 零拷贝：画面已经在显存里了，不用读像素
                if state.dmabuf_buf.is_some() {
                    return;
                }
                let Some((w, h, stride)) = state.geom else {
                    state.failed = Some("缺少画面尺寸".into());
                    return;
                };
                let Some(fd) = state.fd.as_ref() else {
                    state.failed = Some("缺少共享内存".into());
                    return;
                };
                let row = w as usize * 4;
                if (stride as usize) < row {
                    state.failed = Some(format!("行宽异常：stride {stride} 小于 {row}"));
                    return;
                }
                let len = stride as usize * h as usize;
                let mut file = match fd.try_clone() {
                    Ok(f) => std::fs::File::from(f),
                    Err(e) => {
                        state.failed = Some(format!("克隆内存句柄失败: {e}"));
                        return;
                    }
                };
                let mut buf = vec![0u8; len];
                if let Err(e) = file
                    .seek(SeekFrom::Start(0))
                    .and_then(|_| file.read_exact(&mut buf))
                {
                    state.failed = Some(format!("读取画面失败: {e}"));
                    return;
                }
                // 去掉行填充，只留有效像素
                let mut out = if stride as usize == row {
                    buf
                } else {
                    let mut v = Vec::with_capacity(row * h as usize);
                    for line in 0..h as usize {
                        let start = line * stride as usize;
                        v.extend_from_slice(&buf[start..start + row]);
                    }
                    v
                };
                if state.swap_rb {
                    for px in out.chunks_exact_mut(4) {
                        px.swap(0, 2);
                    }
                }
                state.result = Some(out);
            }
            frame::Event::Failed => {
                state.failed = Some("合成器拒绝抓屏".into());
                state.finished = true;
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore ZwlrScreencopyManagerV1);
delegate_noop!(State: ignore zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1);
delegate_noop!(State: ignore zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1);

/// 打开一个 Wayland 连接并绑定需要的全局对象
fn connect() -> Result<(Connection, State, wayland_client::EventQueue<State>), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("连接 Wayland 失败: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("读取全局对象失败: {e}"))?;
    let qh = queue.handle();
    let mut state = State::default();
    for g in globals.contents().clone_list() {
        match g.interface.as_str() {
            "wl_shm" => state.shm = Some(globals.registry().bind(g.name, 1, &qh, ())),
            "wl_output" => {
                // name 事件从 v4 起才有，低版本按合成器给的版本绑
                let v = g.version.min(4);
                let wl: wl_output::WlOutput = globals.registry().bind(g.name, v, &qh, ());
                state.outputs.push(OutputSlot {
                    wl,
                    meta: OutputMeta::default(),
                    have_name: false,
                    done: false,
                });
            }
            "zwlr_screencopy_manager_v1" => {
                let v = g.version.min(3);
                state.manager = Some(globals.registry().bind(g.name, v, &qh, ()));
            }
            "zwp_linux_dmabuf_v1" => {
                // create_immed 要 v3，带 modifier 的 add 也是 v3
                if g.version >= 3 {
                    let v = g.version.min(4);
                    state.dmabuf = Some(globals.registry().bind(g.name, v, &qh, ()));
                }
            }
            _ => {}
        }
    }
    // 两轮往返，确保 wl_output 的 name/transform/scale 都已到齐
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("往返失败: {e}"))?;
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("往返失败: {e}"))?;
    Ok((conn, state, queue))
}

/// 列出所有显示器及其 Wayland 侧信息
pub fn probe() -> Result<Vec<OutputMeta>, String> {
    let (_conn, state, _queue) = connect()?;
    Ok(state
        .outputs
        .iter()
        .filter(|o| o.have_name || o.done)
        .map(|o| o.meta.clone())
        .collect())
}

/// 抓取指定显示器的一帧
pub fn capture_output(name: &str, with_cursor: bool) -> Result<Capture, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let name = name.to_string();
    std::thread::spawn(move || {
        let mut result = capture_blocking(&name, with_cursor, true);
        if result.is_err() {
            // 零拷贝这条路走不通（合成器没报格式、分配不出来、屏是转过的）
            // 就退回共享内存，老路一直在
            eprintln!("零拷贝抓屏不可用，改用共享内存重试");
            result = capture_blocking(&name, with_cursor, false);
        }
        let _ = tx.send(result);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(8)) {
        Ok(r) => r,
        Err(_) => Err("抓屏超时：合成器 5 秒内没有返回画面".into()),
    }
}

fn capture_blocking(name: &str, with_cursor: bool, allow_dmabuf: bool) -> Result<Capture, String> {
    let (_conn, mut state, mut queue) = connect()?;
    let manager = state
        .manager
        .clone()
        .ok_or("合成器不支持 wlr-screencopy")?;
    let _ = state.shm.clone().ok_or("合成器不支持 wl_shm")?;

    let Some(idx) = state.outputs.iter().position(|o| o.meta.name == name) else {
        let names: Vec<String> = state
            .outputs
            .iter()
            .map(|o| o.meta.name.clone())
            .filter(|n| !n.is_empty())
            .collect();
        return Err(format!("找不到显示器 {name}（可用: {}）", names.join(", ")));
    };

    let (output, meta) = {
        let slot = &state.outputs[idx];
        (slot.wl.clone(), slot.meta.clone())
    };
    // 零拷贝只能用在不需要旋转的屏上：dma-buf 里是扫描方向的画面，
    // 转过的屏还得靠 normalize 转一遍
    state.use_dmabuf = allow_dmabuf
        && meta.transform == wl_output::Transform::Normal
        && state.dmabuf.is_some()
        && std::env::var_os("FOCUSSNAPS_NO_DMABUF").is_none();
    let qh = queue.handle();
    let _frame = manager.capture_output(if with_cursor { 1 } else { 0 }, &output, &qh, ());

    while !state.finished {
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("等待抓屏失败: {e}"))?;
    }
    if let Some(err) = state.failed.take() {
        return Err(err);
    }
    if let Some(buf) = state.dmabuf_buf.take() {
        let (width, height) = (buf.width, buf.height);
        return Ok(Capture {
            width,
            height,
            pixels: Vec::new(),
            dmabuf: Some(buf),
        });
    }
    let pixels = state.result.take().ok_or("抓屏没有返回画面")?;
    let (w, h, _) = state.geom.ok_or("缺少画面尺寸")?;
    let (pixels, width, height) =
        normalize(pixels, w as usize, h as usize, meta.transform);
    Ok(Capture {
        width: width as u32,
        height: height as u32,
        pixels,
        dmabuf: None,
    })
}

/// 把合成器给的原始 buffer 旋转成屏幕观感方向
///
/// 合成器返回的是「未旋转」的扫描方向画面，需要按 wl_output 的 transform 反向旋转回来。
fn normalize(
    src: Vec<u8>,
    w: usize,
    h: usize,
    transform: wl_output::Transform,
) -> (Vec<u8>, usize, usize) {
    // 四个翻转变体先对应到「镜像 + 基础旋转」
    let (mirror, base) = match transform {
        wl_output::Transform::Normal => (false, 0),
        wl_output::Transform::_90 => (false, 1),
        wl_output::Transform::_180 => (false, 2),
        wl_output::Transform::_270 => (false, 3),
        wl_output::Transform::Flipped => (true, 0),
        wl_output::Transform::Flipped90 => (true, 1),
        wl_output::Transform::Flipped180 => (true, 2),
        wl_output::Transform::Flipped270 => (true, 3),
        _ => (false, 0),
    };

    let (mut cur, mut cw, ch) = (src, w, h);
    if mirror {
        (cur, cw) = mirror_h(&cur, cw, ch);
    }
    // 合成器把画面逆时针转了 base * 90°，这里转回去
    let (mut out, ow, oh) = match base {
        1 => rotate90_cw(&cur, cw, ch),
        2 => rotate180(&cur, cw, ch),
        3 => rotate90_ccw(&cur, cw, ch),
        _ => (cur, cw, ch),
    };
    // XRGB 的高字节不可信，统一置为不透明
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    // XRGB 的高字节不可信，统一置为不透明
    (out, ow, oh)
}

fn mirror_h(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize) {
    let mut out = vec![0u8; src.len()];
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let s = row + (w - 1 - x) * 4;
            let d = row + x * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    (out, w)
}

fn rotate90_cw(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (h, w);
    let mut out = vec![0u8; src.len()];
    for dy in 0..oh {
        let drow = dy * ow * 4;
        for dx in 0..ow {
            let sx = dy;
            let sy = h - 1 - dx;
            let s = (sy * w + sx) * 4;
            let d = drow + dx * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    (out, ow, oh)
}

fn rotate90_ccw(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (h, w);
    let mut out = vec![0u8; src.len()];
    for dy in 0..oh {
        let drow = dy * ow * 4;
        for dx in 0..ow {
            let sx = w - 1 - dy;
            let sy = dx;
            let s = (sy * w + sx) * 4;
            let d = drow + dx * 4;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    (out, ow, oh)
}

fn rotate180(src: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let mut out = vec![0u8; src.len()];
    let n = w * h;
    for i in 0..n {
        let s = i * 4;
        let d = (n - 1 - i) * 4;
        out[d..d + 4].copy_from_slice(&src[s..s + 4]);
    }
    (out, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张 width*height 的 BGRA 图，R 通道编码像素序号
    fn sample(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for i in 0..w * h {
            v[i * 4] = (i % 251) as u8;
            v[i * 4 + 1] = (i / 251) as u8;
            v[i * 4 + 2] = 0;
            v[i * 4 + 3] = 255;
        }
        v
    }

    fn idx_at(v: &[u8], w: usize, x: usize, y: usize) -> u32 {
        let i = (y * w + x) * 4;
        v[i] as u32 + (v[i + 1] as u32) * 251
    }

    #[test]
    fn rotate90_cw_matches_expected_mapping() {
        // 4 宽 2 高：顺时针转 90° 后是 2 宽 4 高
        let (w, h) = (4usize, 2usize);
        let src = sample(w, h);
        let (out, ow, oh) = rotate90_cw(&src, w, h);
        assert_eq!((ow, oh), (2, 4));
        // 左上角 → 右上角
        assert_eq!(idx_at(&out, ow, 1, 0), 0);
        // 原始 (0,0) 在 (ow-1, 0)
        assert_eq!(idx_at(&out, ow, 0, 0), idx_at(&src, w, 0, h - 1));
    }

    #[test]
    fn rotate90_ccw_roundtrip() {
        let (w, h) = (5usize, 3usize);
        let src = sample(w, h);
        let (a, aw, ah) = rotate90_cw(&src, w, h);
        let (b, bw, bh) = rotate90_ccw(&a, aw, ah);
        assert_eq!((bw, bh), (w, h));
        assert_eq!(b, src);
    }

    #[test]
    fn rotate180_roundtrip() {
        let (w, h) = (5usize, 3usize);
        let src = sample(w, h);
        let (a, _, _) = rotate180(&src, w, h);
        let (b, _, _) = rotate180(&a, w, h);
        assert_eq!(b, src);
    }

    #[test]
    fn normalize_90_swaps_axes_and_forces_opaque() {
        // 旋转屏常见情况：buffer 是扫描方向，宽高与观感方向互换
        let (w, h) = (8usize, 4usize);
        let mut src = sample(w, h);
        for px in src.chunks_exact_mut(4) {
            px[3] = 0; // XRGB 的 X 位不可信
        }
        let (out, ow, oh) = normalize(src, w, h, wl_output::Transform::_90);
        assert_eq!((ow, oh), (4, 8));
        assert!(out.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn normalize_normal_keeps_size() {
        let (w, h) = (8usize, 4usize);
        let src = sample(w, h);
        let (out, ow, oh) = normalize(src.clone(), w, h, wl_output::Transform::Normal);
        assert_eq!((ow, oh), (w, h));
        assert_eq!(out, src);
    }
}
