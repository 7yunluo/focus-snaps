# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目

Wayland（niri / wlroots 系）截图标注工具，Rust + GTK4 + gtk4-layer-shell，单二进制 `FocusSnapS`。用户文档见 `README.md`（含快捷键表、已知限制、坐标与窗口识别原理），改动行为时同步更新它。

## 常用命令

```bash
cargo build --release                    # 构建，产物 target/release/FocusSnapS
cargo run --release                      # 打开浮层（需要真实 Wayland 会话）
cargo run --release -- grab -o DP-2 --png /tmp/a.png   # 无 GUI 抓一帧
cargo run --release -- list              # 显示器 / 旋转 / 逻辑尺寸 / 缩放
cargo test --release                     # 全部测试（无需显示器，秒级）
cargo test --release windows::           # 只跑某个模块：capture:: / export:: / paint:: / overlay:: / windows::
cargo test --release -- detects_two_side_by_side_windows   # 只跑单个测试

cargo install --path . --root ~/.local --force          # 本地安装到 ~/.local/bin
./packaging/build-pkg.sh                                # Arch 包：打 tarball + makepkg -si（要 sudo）
```

`./packaging/build-pkg.sh` 只做本地打包安装；推送、发布等操作必须等七七明确要求。改版本号时 `Cargo.toml` 与 `packaging/PKGBUILD` 的 `pkgver` 要一起改（脚本从 `Cargo.toml` 取版本，PKGBUILD 里是手写常量）。

## 运行模型

`main.rs` 按第一个参数分派：`grab` / `full` / `list` / `detect` 是纯命令行子命令，其余进入 `overlay::run` 打开浮层。命令行解析是手写的（`Args::value` / `Args::flag` / `check_known`），没有 clap。

浮层模式流程：`overlay::build` → 枚举 GDK 显示器 → 对每个显示器 `capture::capture_output` 抓一帧 + `windows::detect_for_output` 识别窗口 → 组装 `Doc` → 每屏建一个 layer-shell 窗口。退出条件是 `app.quit()`（复制 / 保存 / Esc 后都走这里）。

## 三层坐标系（最容易踩的地方）

1. **逻辑坐标**：GDK `monitor.geometry()`，浮层窗口、`Selection`、`Stroke.points`、`Rect` 全用它
2. **观感物理像素**：逻辑 × `OutputLayer.sx/sy`，`sx = 抓到的画面宽 / GDK 逻辑宽`（不是 `wl_output.scale`，分数缩放会不同）
3. **扫描方向像素**：`wlr-screencopy` 原始 buffer，被旋转的输出上宽高是反的

`capture::normalize` 把 2→3 方向反过来；`paint` 绘制时 `cr.scale(1/sx, 1/sy)` 让画面按物理像素 1:1 采样；`export::render_rect` 直接按物理分辨率建画布。新增任何与像素尺寸相关的逻辑，先确认自己在哪一层。

## 状态与多屏

- `Doc`（`doc.rs`）是唯一状态：`outs`（每屏画面 + 缩放 + 窗口矩形）、`sel`、`strokes`、`redo`、`active`（拖拽中的临时笔画）、`hover`、`tool` / `color` / `width`。放在 `Rc<RefCell<Doc>>`（`ui::Shared`）里被所有屏共享。
- `Selection.out` 指明选区属于哪块屏；**选区限定在单屏内**，跨屏不支持。重新框选会清空标注（标注属于那一次选区）。改选区 / 工具的路径都要跟着想一遍 `strokes` / `redo` / `next_number`。
- 交互在 `overlay.rs`：`Session` 编排所有窗口，`DragKind`（`NewSel` / `Resize(handle)` / `Move` / `Draw`）是拖拽状态机，`on_drag_begin/update/end` 三段修改 `Doc`，`DragState.saved_*` 用于取消时还原。`Session::redraw` 只重绘选区或悬停所属的那块屏，切屏时全画。
- 键盘：niri 只把键交给指针所在输出上的独占层表面，所以每屏都 `KeyboardMode::Exclusive`，指针 enter 时 `focus_window` 重新申请；快捷键控制器挂在窗口上且用 `PropagationPhase::Capture`，文字输入框 mapped 时整体让路。

## 绘制与导出一致性

`paint::draw_stroke` 同时被浮层绘制（`paint::paint`）和导出渲染（`export::render_rect`）调用。**新增工具或改画法必须改这里**，否则浮层看到的效果和落盘图不一致。马赛克是特例：它直接从 `OutputLayer.phys` 采样求块平均色。

导出一律走 `export::render_selection` → `save_to_dir` / `save_to_path`（PNG 由 `image` crate 编码）；复制先试 `wl-copy`（不依赖键盘焦点），失败退回 GDK 剪贴板纹理。

## 窗口识别

`windows.rs` 的核心不是阈值调参，而是「IPC 先验 + 一维搜索」：

- `niri.rs` 用 `niri msg --json workspaces/windows` 取当前活动工作区的窗口 `window_size` / `tile_size` / 列号 / `window_offset_in_tile`；`gaps()` 从 `~/.config/niri/layout.kdl`（或 `config.kdl`）读间隙，读不到用 10。
- niri 不告诉客户端平铺窗口的滚动偏移（`tile_pos_in_workspace_view` 对平铺窗口恒为 `None`），`solve_offset` 拿列宽当先验，在整屏范围扫这个唯一未知量，取「所有列边界都落在画面强竖边上」的偏移；`horizontal_bounds` 补上下边。
- 拿不到 IPC 或对不上画面时退回纯图像识别 `detect`（逐列纵向对比度 → 边界带 → 窗口）。
- `FocusSnapS detect -o X [--png out.png] [--cols 900,900] [--file 图.png]` 是这套算法的排障入口，`--cols` / `--file` 可跳过 IPC 离线复算；`solve_offset_pub` 就是给它用的。

## 抓屏

`capture.rs` 直接用 `wayland-client` 绑 `wl_output` / `wl_shm` / `zwlr_screencopy_manager_v1`，写 memfd、去掉行填充、按 `wl_output.transform` 旋转（含 4 种翻转变体）、alpha 强制 255。抓屏在独立线程里跑，5 秒超时。它与 GTK 无关，`list` / `detect` 里另外用 GTK 取逻辑几何。

## 测试

测试分布在各模块的 `#[cfg(test)]` 里，全部不需要显示器（cairo `ImageSurface` 可离屏画）。造数据的方式是自造像素面并在通道里编码坐标（`export::tests::layer` 用 R=x/G=y，`windows::tests` 合成窗口画面），比对时按这个映射断言。改坐标映射、旋转、缩放、识别算法时优先补/改这类表驱动测试。

另外有一组环境变量测试钩子（`FOCUSSNAPS_TEST_SELECT` / `_ANNOTATE` / `_SAVE` / `_POINTER`）驱动真实浮层自动跑一遍，见 README。

## 代码约定

- 注释与文档用中文，注释只写「这段是干什么用的」；机制细节、踩坑史、实测数字写进 `README.md`。
- cairo 的返回值统一 `let _ =` 忽略，沿用现有风格。
- 需要保留的调试输出用 `eprintln!`，加在环境变量开关（如 `FOCUSSNAPS_DEBUG_EDGES`）后面。
