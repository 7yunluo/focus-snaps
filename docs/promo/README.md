# 宣传图

`../focus-snaps-promo.png`（1600×1250）给 README 用，
`focus-snaps-promo-1280x640.png` 给 GitHub 仓库的 Social preview 用（Settings → General → Social preview）。

都是 `build.sh` 拼出来的：

```
├── mock-desktop.svg    演示用的假桌面（两个窗口的示意图，不含真实屏幕内容）
├── shot.png            真截图：FocusSnapS 在上面那个假桌面上开的浮层
├── promo.svg           1600×1250 版排版
├── promo-wide.svg      1280×640 版排版
├── logo.png            应用图标位图
└── build.sh            拼图脚本
```

重新拼图（改了排版/文案，只需 `rsvg-convert` 和 ImageMagick）：

```bash
./build.sh
```

重截演示画面（改了浮层样式、标注位置时要重跑，会临时全屏占用显示器）：

```bash
./build.sh --capture
```

`--capture` 依赖 `mpv`（全屏显示假桌面）和已安装的 `FocusSnapS`，用的是三个测试钩子：
`FOCUSSNAPS_TEST_SELECT` 定选区、`FOCUSSNAPS_TEST_ANNOTATE=promo` 画成套示例标注、
`FOCUSSNAPS_TEST_PALETTE` 展开调色板。标注的坐标写在 `src/overlay.rs` 的 promo 分支里，
换演示内容要跟着改那几行。
