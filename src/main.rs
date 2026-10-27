// 命令行入口
mod capture;
mod doc;
mod export;
mod overlay;
mod paint;
mod ui;
mod niri;
mod windows;

use std::io::Write;

fn help() {
    println!(
        "FocusSnapS {}\n\n\
         用法:\n  \
         FocusSnapS [--output 显示器] [--cursor]   打开截图浮层（选区 + 标注）\n  \
         FocusSnapS grab -o 显示器 [--png 路径] [--cursor]\n                                              抓取一帧，默认以 8 字节头 + BGRA 写 stdout\n  \
         FocusSnapS full [-o 显示器] [--png 路径] [--cursor] [--delay 秒]\n                                              直接整屏保存\n  \
         FocusSnapS list                          列出显示器与缩放信息\n  \
         FocusSnapS detect -o 显示器 [--png 路径]   试跑窗口识别（排障用）\n  \
         FocusSnapS --version | --help\n\n\
         浮层快捷键:\n  \
         单击窗口        选中整块窗口（悬停时有高亮预览）\n  \
         拖动            自由框选区域\n  \
         回车 / 空格 / 双击           复制到剪贴板（默认完成动作）\n  \
         Ctrl+S / Ctrl+C             保存文件 / 复制    Esc 退出\n  \
         拖动工具条左侧 ⠿  移动工具条\n  \
         Ctrl+A          选中整块屏幕    Ctrl+Z / Ctrl+Shift+Z 撤销 / 重做\n  \
         V 选择  B 画笔  H 高亮  L 直线  A 箭头  R 矩形  E 椭圆  T 文字  N 序号  M 马赛克\n  \
         方向键微调选区（Shift 加速）    右键 清空选区 / 退出",
        env!("CARGO_PKG_VERSION")
    );
}

struct Args {
    /// 去掉子命令后的剩余参数
    rest: Vec<String>,
}

impl Args {
    fn value(&self, keys: &[&str]) -> Option<String> {
        let mut i = 0;
        while i < self.rest.len() {
            if keys.contains(&self.rest[i].as_str()) {
                return self.rest.get(i + 1).cloned();
            }
            i += 1;
        }
        None
    }
    fn flag(&self, keys: &[&str]) -> bool {
        self.rest.iter().any(|a| keys.contains(&a.as_str()))
    }
}

/// 未知参数检查
fn check_known(args: &[String], with_value: &[&str], flags: &[&str]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if with_value.contains(&a) {
            if args.get(i + 1).is_none() {
                return Some(format!("参数 {a} 缺少取值"));
            }
            i += 2;
            continue;
        }
        if flags.contains(&a) {
            i += 1;
            continue;
        }
        return Some(format!("未知参数: {a}"));
    }
    None
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match argv.first().map(|s| s.as_str()) {
        Some("grab") => cmd_grab(&argv[1..]),
        Some("full") => cmd_full(&argv[1..]),
        Some("list") => cmd_list(&argv[1..]),
        Some("detect") => cmd_detect(&argv[1..]),
        Some("--version") | Some("-V") => {
            println!("FocusSnapS {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--help") | Some("-h") | Some("help") => {
            help();
            0
        }
        _ => cmd_overlay(&argv),
    };
    std::process::exit(code);
}

fn cmd_overlay(argv: &[String]) -> i32 {
    let args = Args {
        rest: argv.to_vec(),
    };
    if let Some(err) = check_known(
        argv,
        &["--output", "-o"],
        &["--cursor", "--no-cursor"],
    ) {
        eprintln!("{err}");
        return 2;
    }
    let opts = overlay::Options {
        only: args.value(&["--output", "-o"]),
        cursor: args.flag(&["--cursor"]),
    };
    overlay::run(opts)
}

fn cmd_grab(argv: &[String]) -> i32 {
    let args = Args {
        rest: argv.to_vec(),
    };
    if let Some(err) = check_known(
        argv,
        &["--output", "-o", "--png"],
        &["--cursor", "--no-cursor"],
    ) {
        eprintln!("{err}");
        return 2;
    }
    let Some(name) = args.value(&["--output", "-o"]) else {
        eprintln!("用法: FocusSnapS grab -o 显示器名 [--png 路径] [--cursor]");
        return 2;
    };
    let cap = match capture::capture_output(&name, args.flag(&["--cursor"])) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("抓屏失败: {e}");
            return 1;
        }
    };
    if let Some(path) = args.value(&["--png"]) {
        let mut surface = match export::surface_from_bgra(cap.width, cap.height, &cap.pixels) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        };
        return match export::save_to_path(&mut surface, &path) {
            Ok(p) => {
                println!("{}x{} {}", cap.width, cap.height, p.display());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&cap.width.to_le_bytes()).is_err()
        || stdout.write_all(&cap.height.to_le_bytes()).is_err()
        || stdout.write_all(&cap.pixels).is_err()
    {
        eprintln!("写 stdout 失败");
        return 1;
    }
    0
}

fn cmd_full(argv: &[String]) -> i32 {
    let args = Args {
        rest: argv.to_vec(),
    };
    if let Some(err) = check_known(
        argv,
        &["--output", "-o", "--png", "--delay"],
        &["--cursor", "--no-cursor"],
    ) {
        eprintln!("{err}");
        return 2;
    }
    if let Some(secs) = args.value(&["--delay"]).and_then(|v| v.parse::<f64>().ok()) {
        if secs > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(secs));
        }
    }
    let name = match args.value(&["--output", "-o"]) {
        Some(n) => n,
        None => match focused_output().or_else(|| {
            capture::probe()
                .ok()
                .and_then(|v| v.into_iter().next().map(|m| m.name))
        }) {
            Some(n) => n,
            None => {
                eprintln!("没有找到显示器");
                return 1;
            }
        },
    };
    let cap = match capture::capture_output(&name, args.flag(&["--cursor"])) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("抓屏失败: {e}");
            return 1;
        }
    };
    let mut surface = match export::surface_from_bgra(cap.width, cap.height, &cap.pixels) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let result = match args.value(&["--png"]) {
        Some(path) => export::save_to_path(&mut surface, &path),
        None => export::save_to_dir(&mut surface, &doc::default_dir()),
    };
    match result {
        Ok(path) => {
            println!("{}", path.display());
            export::notify("截图已保存", &path.display().to_string());
            0
        }
        Err(e) => {
            eprintln!("保存失败: {e}");
            1
        }
    }
}

/// 试跑窗口识别，把结果画到图上便于核对
fn cmd_detect(argv: &[String]) -> i32 {
    let args = Args {
        rest: argv.to_vec(),
    };
    if let Some(err) = check_known(argv, &["--output", "-o", "--png", "--file", "--cols"], &[]) {
        eprintln!("{err}");
        return 2;
    }
    let Some(name) = args.value(&["--output", "-o"]) else {
        eprintln!("用法: FocusSnapS detect -o 显示器名 [--png 路径] [--file 图.png] [--cols 900,900]");
        return 2;
    };
    // --file 直接分析现成的 PNG，配合 --cols 可以离线验证布局推算
    let mut surface = if let Some(path) = args.value(&["--file"]) {
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let mut s = match gtk::cairo::ImageSurface::create(
                    gtk::cairo::Format::ARgb32,
                    w as i32,
                    h as i32,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("建画布失败: {e}");
                        return 1;
                    }
                };
                {
                    let stride = s.stride() as usize;
                    let mut data = s.data().unwrap();
                    for y in 0..h as usize {
                        for x in 0..w as usize {
                            let src = &rgba.as_raw()[(y * w as usize + x) * 4..][..4];
                            let dst = y * stride + x * 4;
                            data[dst] = src[2];
                            data[dst + 1] = src[1];
                            data[dst + 2] = src[0];
                            data[dst + 3] = 255;
                        }
                    }
                }
                s.mark_dirty();
                s
            }
            Err(e) => {
                eprintln!("读取 {path} 失败: {e}");
                return 1;
            }
        }
    } else {
        let cap = match capture::capture_output(&name, false) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("抓屏失败: {e}");
                return 1;
            }
        };
        match export::surface_from_bgra(cap.width, cap.height, &cap.pixels) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    };
    // 逻辑尺寸从 GDK 拿
    let mut logical = (
        surface.width() as f64,
        surface.height() as f64,
    );
    let mut logical_known = false;
    let _ = &mut logical_known;
    if gtk::init().is_ok() {
        if let Some(display) = gtk::gdk::Display::default() {
            use gtk::prelude::*;
            let model = display.monitors();
            for i in 0..model.n_items() {
                if let Some(obj) = model.item(i) {
                    if let Ok(m) = obj.downcast::<gtk::gdk::Monitor>() {
                        if m.connector().map(|c| c.to_string()).as_deref() == Some(name.as_str()) {
                            let g = m.geometry();
                            logical = (f64::from(g.width()), f64::from(g.height()));
                            logical_known = true;
                        }
                    }
                }
            }
        }
    }
    let _ = logical_known;
    let sx = surface.width() as f64 / logical.0;
    let sy = surface.height() as f64 / logical.1;
    let rects = if let Some(spec) = args.value(&["--cols"]) {
        // 离线：手给列宽，跳过 IPC
        let plan: Vec<(f64, f64)> = spec
            .split(',')
            .filter_map(|v| v.trim().parse::<f64>().ok())
            .map(|w| (w, w))
            .collect();
        windows::solve_offset_pub(&mut surface, sx, sy, logical, &plan).unwrap_or_default()
    } else {
        windows::detect_for_output(&name, &mut surface, sx, sy, logical)
    };
    println!("{name} 逻辑尺寸 {:.0}x{:.0} 缩放 {sx:.3}x{sy:.3}", logical.0, logical.1);
    for (i, r) in rects.iter().enumerate() {
        println!(
            "  #{i} x {:.0}..{:.0} y {:.0}..{:.0}  ({:.0}x{:.0})",
            r.x0,
            r.x1,
            r.y0,
            r.y1,
            r.width(),
            r.height()
        );
    }
    if rects.is_empty() {
        println!("  （没识别到窗口）");
    }
    if let Some(path) = args.value(&["--png"]) {
        if let Ok(cr) = gtk::cairo::Context::new(&surface) {
            cr.scale(sx, sy);
            for (i, r) in rects.iter().enumerate() {
                cr.set_source_rgba(1.0, 0.3, 0.2, 0.9);
                cr.set_line_width(2.0);
                cr.rectangle(r.x0, r.y0, r.width(), r.height());
                let _ = cr.stroke();
                cr.move_to(r.x0 + 8.0, r.y0 + 28.0);
                cr.set_font_size(28.0);
                let _ = cr.show_text(&format!("#{i}"));
            }
            surface.flush();
        }
        match export::save_to_path(&mut surface, &path) {
            Ok(p) => println!("标注图 -> {}", p.display()),
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    }
    0
}

/// 优先用 niri 报告的焦点显示器，拿不到就返回 None
fn focused_output() -> Option<String> {
    let out = std::process::Command::new("niri")
        .args(["msg", "--json", "focused-output"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let key = "\"name\":\"";
    let start = text.find(key)? + key.len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn cmd_list(argv: &[String]) -> i32 {
    if let Some(err) = check_known(argv, &[], &[]) {
        eprintln!("{err}");
        return 2;
    }
    let metas = capture::probe().unwrap_or_default();
    if metas.is_empty() {
        eprintln!("没有探测到任何 Wayland 显示器");
        return 1;
    }
    // GDK 视角（逻辑坐标与缩放）需要初始化 GTK
    let mut gdk_info: Vec<(String, i32, i32, i32, i32)> = Vec::new();
    if gtk::init().is_ok() {
        if let Some(display) = gtk::gdk::Display::default() {
            use gtk::prelude::*;
            let model = display.monitors();
            for i in 0..model.n_items() {
                if let Some(obj) = model.item(i) {
                    if let Ok(m) = obj.downcast::<gtk::gdk::Monitor>() {
                        let g = m.geometry();
                        let name = m.connector().map(|s| s.to_string()).unwrap_or_default();
                        gdk_info.push((name, g.x(), g.y(), g.width(), g.height()));
                    }
                }
            }
        }
    }
    for m in &metas {
        let logical = gdk_info.iter().find(|(n, ..)| *n == m.name);
        let rotate = matches!(
            m.transform,
            wayland_client::protocol::wl_output::Transform::_90
                | wayland_client::protocol::wl_output::Transform::_270
                | wayland_client::protocol::wl_output::Transform::Flipped90
                | wayland_client::protocol::wl_output::Transform::Flipped270
        );
        let (phys_w, phys_h) = if rotate {
            (m.mode.1, m.mode.0)
        } else {
            m.mode
        };
        let (sx, sy) = logical
            .map(|(_, _, _, w, h)| (phys_w as f64 / *w as f64, phys_h as f64 / *h as f64))
            .unwrap_or((m.scale as f64, m.scale as f64));
        println!("{}", m.name);
        println!(
            "  模式 {}x{}  旋转 {:?}  观感 {}x{}",
            m.mode.0, m.mode.1, m.transform, phys_w, phys_h
        );
        match logical {
            Some((_, x, y, w, h)) => {
                println!("  逻辑 {}x{} @ ({x}, {y})  缩放 {sx:.3}x{sy:.3}", w, h)
            }
            None => println!("  （GDK 未提供逻辑几何，缩放估算 {sx:.3}）"),
        }
    }
    0
}
