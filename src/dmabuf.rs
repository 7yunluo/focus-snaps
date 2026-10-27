// 零拷贝抓屏用的显存缓冲
//
// 合成器把画面直接画进这块显存（dma-buf），CPU 一个字节都不用读回来；
// 需要像素的时候（导出、打码、窗口识别）再 mmap 读，是纯内存拷贝而不是
// 「等 GPU 停下再读」。
//
// 分配走 DRM 的 dumb buffer：线性排布，所有驱动都认，而且只能在线性排布下
// 才能 mmap。路子照 ../farwing 的 capture/dmabuf.rs。
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};

use wayland_client::protocol::wl_buffer;
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};

/// 线性排布，dumb buffer 一定是它
const LINEAR: u64 = 0;

/// ioctl 请求号：`_IOWR('d', nr, type)`
const fn request(number: u64, size: u64) -> std::ffi::c_ulong {
    const WRITE: u64 = 1;
    const READ: u64 = 2;
    (((READ | WRITE) << 30) | (size << 16) | ((b'd' as u64) << 8) | number) as std::ffi::c_ulong
}

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct DestroyDumb {
    handle: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

const CREATE_DUMB: std::ffi::c_ulong = request(0xb2, size_of::<CreateDumb>() as u64);
const MAP_DUMB: std::ffi::c_ulong = request(0xb3, size_of::<MapDumb>() as u64);
const DESTROY_DUMB: std::ffi::c_ulong = request(0xb4, size_of::<DestroyDumb>() as u64);
const HANDLE_TO_FD: std::ffi::c_ulong = request(0x2d, size_of::<PrimeHandle>() as u64);

/// 一块交给合成器画的显存缓冲
pub struct Buffer {
    /// 导出给合成器和 GDK 的 fd
    pub prime: OwnedFd,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub modifier: u64,
    size: u64,
    handle: u32,
    card: OwnedFd,
}

impl Buffer {
    /// 在显卡上分配一块线性缓冲，格式用合成器在 `linux_dmabuf` 事件里报的那个
    pub fn allocate(width: u32, height: u32, format: u32) -> Result<Self, String> {
        let card = card_node()?;
        let mut create = CreateDumb {
            height,
            width,
            bpp: 32,
            ..Default::default()
        };
        // SAFETY: card 是打开的 DRM 节点，结构体布局与 ioctl 要求一致
        let rc = unsafe { libc::ioctl(card.as_raw_fd(), CREATE_DUMB, &mut create) };
        if rc < 0 {
            return Err(format!("分配显存失败: {}", std::io::Error::last_os_error()));
        }
        if create.handle == 0 || create.size == 0 {
            return Err("驱动没有给出缓冲句柄".into());
        }
        let mut prime = PrimeHandle {
            handle: create.handle,
            flags: 0,
            fd: -1,
        };
        // SAFETY: handle 来自这个节点，结构体布局与 ioctl 要求一致
        let rc = unsafe { libc::ioctl(card.as_raw_fd(), HANDLE_TO_FD, &mut prime) };
        if rc < 0 || prime.fd < 0 {
            let error = std::io::Error::last_os_error();
            destroy(card.as_raw_fd(), create.handle);
            return Err(format!("导出显存 fd 失败: {error}"));
        }
        // SAFETY: 内核刚创建了这条 fd，没有别的地方持有它
        let prime = unsafe { OwnedFd::from_raw_fd(prime.fd) };
        Ok(Self {
            prime,
            width,
            height,
            stride: create.pitch,
            format,
            modifier: LINEAR,
            size: create.size,
            handle: create.handle,
            card,
        })
    }

    /// 包成合成器认的 `wl_buffer`
    pub fn wrap<S>(&self, dmabuf: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, qh: &QueueHandle<S>) -> wl_buffer::WlBuffer
    where
        S: Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, ()>
            + Dispatch<wl_buffer::WlBuffer, ()>
            + 'static,
    {
        let params = dmabuf.create_params(qh, ());
        params.add(
            self.prime.as_fd(),
            0,
            0,
            self.stride,
            (self.modifier >> 32) as u32,
            (self.modifier & 0xffff_ffff) as u32,
        );
        // create_immed 是同步那一支：直接拿到 wl_buffer，不用等 created 事件
        let buffer = params.create_immed(
            self.width as i32,
            self.height as i32,
            self.format,
            zwp_linux_buffer_params_v1::Flags::empty(),
            qh,
            (),
        );
        params.destroy();
        buffer
    }


    /// 拿一份可以搬到别的线程去的读像素任务
    ///
    /// 读显存通常慢（write-combined 映射），所以这件事不该占主线程，
    /// 也不该拦着画面出现——纹理那边是零拷贝，本来就不需要像素。
    pub fn reader(&self) -> Reader {
        Reader {
            card: self.card.try_clone().ok(),
            handle: self.handle,
            size: self.size,
            width: self.width,
            height: self.height,
            stride: self.stride,
        }
    }
}

/// 在后台线程里把显存内容读成紧密排列的 BGRA 像素
pub struct Reader {
    card: Option<OwnedFd>,
    handle: u32,
    size: u64,
    pub width: u32,
    pub height: u32,
    stride: u32,
}

impl Reader {
    pub fn read_pixels(&self) -> Result<Vec<u8>, String> {
        let card = self.card.as_ref().ok_or("显存句柄已经关掉了")?;
        let mut map = MapDumb {
            handle: self.handle,
            ..Default::default()
        };
        // SAFETY: handle 是本缓冲的，结构体布局与 ioctl 要求一致
        let rc = unsafe { libc::ioctl(card.as_raw_fd(), MAP_DUMB, &mut map) };
        if rc < 0 {
            return Err(format!("映射显存失败: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: 驱动给了这块缓冲的偏移，映射大小是驱动报的 size
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                self.size as usize,
                libc::PROT_READ,
                libc::MAP_SHARED,
                card.as_raw_fd(),
                map.offset as i64,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!("mmap 显存失败: {}", std::io::Error::last_os_error()));
        }
        let row = self.width as usize * 4;
        let stride = self.stride as usize;
        let mut out = Vec::with_capacity(row * self.height as usize);
        // SAFETY: 映射有 size 字节，逐行读取都在范围内
        unsafe {
            let base = ptr as *const u8;
            for y in 0..self.height as usize {
                let line = std::slice::from_raw_parts(base.add(y * stride), row);
                out.extend_from_slice(line);
            }
            libc::munmap(ptr, self.size as usize);
        }
        Ok(out)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        destroy(self.card.as_raw_fd(), self.handle);
    }
}

fn destroy(card: RawFd, handle: u32) {
    if handle == 0 {
        return;
    }
    let mut request = DestroyDumb { handle, pad: 0 };
    // SAFETY: handle 是我们的，结构体布局与 ioctl 要求一致
    unsafe { libc::ioctl(card, DESTROY_DUMB, &mut request) };
}

/// 找驱动着显示器的 card 节点
///
/// dumb buffer 只能在 `cardN` 上分配：`renderD*` 上这类 ioctl 会 EACCES。
fn card_node() -> Result<OwnedFd, String> {
    let entries = std::fs::read_dir("/sys/class/drm").map_err(|e| format!("读 /sys/class/drm 失败: {e}"))?;
    let mut tried = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // 只要 cardN，跳过 cardN-XXX 这类连接器目录
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let path = format!("/dev/dri/{name}");
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => return Ok(file.into()),
            Err(e) => tried.push(format!("{path}: {e}")),
        }
    }
    Err(format!("没有可用的 DRM card 节点（{}）", tried.join(", ")))
}
