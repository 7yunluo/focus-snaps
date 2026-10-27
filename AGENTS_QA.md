# QA / E2E 踩坑记录

跑跨屏浮层相关验证时踩到的问题，动手前先看一眼。

## 单实例锁会让新启动静默失败

残留进程会占住 `$XDG_RUNTIME_DIR/FocusSnapS.lock`，新进程只往 stderr 打一行
`已经有一个截图浮层开着` 就退出。屏幕上看不到任何浮层，但用 `grab` 抓到的
却是上一轮残留实例的画面，很容易误判成「绘制没生效」。

开测前一律先 `pkill -x FocusSnapS`，并确认 `pgrep -a FocusSnapS` 为空。

## 别用 `pkill -f` 匹配本项目路径

`pkill -f debug/FocusSnapS` 会连自己所在的 shell 命令行一起匹配，shell 被
SIGTERM 掉，命令以退出码 143/144 中断。用 `pkill -x FocusSnapS`（按进程名精确匹配）。

## 用 grab 抓浮层本身

`FocusSnapS grab -o <屏> --png x.png` 走 screencopy，抓的就是当前屏幕内容，
能直接看到浮层画出来的压暗、选区、工具条。

```sh
pkill -x FocusSnapS
FOCUSSNAPS_TEST_SELECT="700,300,600,400" ./target/debug/FocusSnapS &
sleep 4
./target/debug/FocusSnapS grab -o DP-3 --png /tmp/a.png
./target/debug/FocusSnapS grab -o DP-1 --png /tmp/b.png
pkill -x FocusSnapS
python3 -c "from PIL import Image; im=Image.open('/tmp/a.png'); im.thumbnail((520,520)); im.save('/tmp/sa.png')"
```

`FOCUSSNAPS_TEST_SELECT` 现在按**全局逻辑坐标**解释（多屏拼起来的布局空间），
跨屏用例给一个比单屏更宽的矩形即可。

## 只验导出、不开浮层最省事

```sh
FOCUSSNAPS_TEST_SELECT="900,100,200,100" FOCUSSNAPS_TEST_SAVE=/tmp/x.png ./target/debug/FocusSnapS
```

进程存完自己退出，PNG 尺寸应等于「逻辑宽 × 参考缩放」，参考缩放取各屏里最大的一档。

## 本机布局（可直接拿来写跨屏用例）

| 屏 | 逻辑尺寸 | 位置 | 缩放 |
| --- | --- | --- | --- |
| DP-3 | 1000x1600 | (0, 0) | 1.6 |
| DP-1 | 3793x1600 | (1000, 0) | 1.35 |

`./target/debug/FocusSnapS list` 随时可以重查。

## 用 ydotool 模拟拖拽不靠谱

`ydotool mousemove -a` 的绝对坐标要经过加速/映射换算，落点对不上；相对移动也会
被指针加速带偏，跨屏拖动模拟很难复现。这类交互还是人工拖一次更快。

## 抓 cairo 画布的像素，别留着多余的引用

`ImageSurface::data()` 要求这个画布只有一份引用，否则报
`Can't get exclusive access`，然后你会看到打码 / 导出一类功能静默失效。
以前 `paint()` 为了贴图 `clone()` 了一份画面，正好把这个坑埋在那儿：
打码那一步拿不到像素，块平均直接跳过了。

## 浮层视觉回归怎么验（不靠肉眼）

抓一张带浮层的图、一张不带的图，逐像素算比值：

```sh
./target/debug/FocusSnapS grab -o DP-3 --png /tmp/with.png     # 浮层开着
./target/debug/FocusSnapS grab -o DP-3 --png /tmp/without.png  # 浮层关掉
python3 -c "
import numpy as np; from PIL import Image
a=np.asarray(Image.open('/tmp/with.png').convert('L'),float)
b=np.asarray(Image.open('/tmp/without.png').convert('L'),float)
r=a/np.maximum(b,1)
print('压暗区比值中位数', np.median(r[b>90]))"
```

压暗用的是 45% 黑，sRGB 混合，**比值应当是个常数 0.550**（不管是 cairo 画的还是
CSS 矩形铺的）。比值对了就说明压暗几何没坏。别拿「被压暗的像素占比」当硬指标：
桌面内容每次都在变，那个数会飘。

跨运行的截图没法逐像素比对——状态栏时钟、聊天窗口都在动。

## 想知道指针事件落在哪一层，用 Gtk.Widget.pick()

不用折腾 ydotool。照真实层级在 python 里搭个小 repro，`win.pick(x, y, 0)`
看返回哪个 widget 就行。`set_can_target(False)` 的层会被跳过，所以能确认
「压暗层/标注层不吃事件、手势层接得到」。

## 每帧成本怎么量（帧率是假的）

帧率被 vsync 钉死（200 Hz 屏就是 5 ms 一帧），`FOCUSSNAPS_BENCH` 只能告诉你
「追不追得上刷新率」。要量真实成本，跑两次不同帧数、取两次的 user+sys 相减：

```sh
TIMEFORMAT='%R'
time FOCUSSNAPS_BENCH=100 FOCUSSNAPS_TEST_SELECT="100,300,600,400" ./target/release/FocusSnapS -o DP-1
time FOCUSSNAPS_BENCH=400 FOCUSSNAPS_TEST_SELECT="100,300,600,400" ./target/release/FocusSnapS -o DP-1
# (第二次 CPU 时间 - 第一次) / 300 = 每帧 CPU
```

重构前后的实测：DP-1（5120x2160）6.2 ms/帧 → 0.07 ms/帧，
DP-3（1600x2560）2.4 ms/帧 → 0.33 ms/帧。

## 对照版本用 git worktree 建，别在同一棵树里来回切

```sh
git worktree add /tmp/fs-old <旧提交>
cd /tmp/fs-old && CARGO_TARGET_DIR=/tmp/fs-old-target cargo build --release
```

基准钩子（`FOCUSSNAPS_BENCH`）在两个树里都加上，跑同一组参数才可比。

## 零拷贝抓屏（dmabuf）与它的坑

`zwlr_screencopy` 走 wl_shm 时，合成器要把整屏画面从显存读回共享内存
（DP-1 是 44 MB），GPU 忙的时候这一步能卡到 1.5~3 s，`FOCUSSNAPS_TRACE=1`
一眼就能看出来（`抓屏完` 那一行）。

零拷贝那条路照 `../farwing/src/platform/linux/capture/dmabuf.rs`：
DRM dumb buffer（`CREATE_DUMB` + `HANDLE_TO_FD`）+ `zwp_linux_dmabuf_v1`
的 `create_immed`，再用 `gdk::DmabufTextureBuilder` 直接导成 `GdkTexture`。
实测抓屏 1500 ms → 20~40 ms。

几个坑：

- **dumb buffer 只能在 `/dev/dri/cardN` 上分配**，`renderD*` 会 EACCES
- **mmap 读显存很慢**：驱动给的是 write-combined 映射，44 MB 读下来能到
  1 s 量级。所以像素读取要放在独立线程，别占主线程，也别拦着画面出现；
  只想要「显示」的话根本不需要 CPU 像素
- **转过的屏走不了这条路**（dma-buf 里是扫描方向），DP-3 是 90°，自动退回共享内存
- 抓屏出来的 alpha 都是 255，纹理按 `B8G8R8A8_PREMULTIPLIED` 处理与直通一致

对照验证：

```sh
# 零拷贝 vs 共享内存，导出的 PNG 应当逐像素一致
FOCUSSNAPS_DIR=/tmp/d1 FOCUSSNAPS_TEST_SELECT="1500,300,600,400" \
  FOCUSSNAPS_TEST_SAVE="@" ./target/release/FocusSnapS -o DP-1
FOCUSSNAPS_NO_DMABUF=1 FOCUSSNAPS_DIR=/tmp/d2 FOCUSSNAPS_TEST_SELECT="1500,300,600,400" \
  FOCUSSNAPS_TEST_SAVE="@" ./target/release/FocusSnapS -o DP-1
```

两次必须**紧挨着跑**：桌面内容（时钟、日志滚动）一变，比值就没意义了。

## 浮层先出现、画面后补

抓屏慢的时候不能拦着浮层出现。现在 `build()` 只按显示器位置建图层并立刻
present（本机 +22 ms），画面由后台线程抓到一块补一块。没画面之前窗口带
`.fs-pending`（透明），桌面透出来只叠一层压暗。

导出要等画面齐（`all_ready()` 看的是画布，不只是纹理），这期间按下的
复制/保存会记在 `Session.pending` 里，最后一块屏补齐后自动执行。

## 验证浮层要摆正三件事：坐标、时机、以及别让内容在动

踩过的坑一次说清：

- **`FOCUSSNAPS_TEST_SELECT` 是全局布局坐标**，不是本屏局部坐标。本机 DP-1 在
  全局 `x=1000`（`niri msg --json outputs` 能看到 `logical`），所以「本屏局部
  (1100,900)」要写成 `2100,900`。写错的话截出来的是另一块地方，怎么看都不对。
- **`FOCUSSNAPS_TEST_SAVE` 存完会直接退出应用**（`test_save` 里有 `app.quit()`），
  所以别指望在同一次运行里既落盘又抓浮层。要两样就分两次跑。
- **拿「和普通截图比」当对齐指标时，内容必须是静止的**。本机的终端一直在刷，
  冻结的画面和实时画面根本对不上，残差永远是 20 以上，会把人带沟里。挑屏幕上
  静止的那块（比如左边那份文档）来测，或者接受「残差只说明内容变了」这件事。
- **抓图用 `FocusSnapS grab --png`**，它抓的是合成后的输出，浮层在里面。

## 亮洞（选区里那块没压暗的画面）

实现是 `src/hole.rs` 里一个自定义 widget，把整屏纹理当成一个纹理节点画出来，
位置靠 `(-at)` 摊开、自己 `push_clip` 裁到控件大小。要在截图里验证它对不对，
比两件事：

- **压暗比值**：同一次运行里 `overlay.png` 和 `plain.png` 的整体均值之比应当
  0.55~0.58（45% 黑压暗，sRGB 直接混），而**洞里的比值应当是 1.000**。
- **洞里的画面和普通截图同一处对不对得上**：拿洞里中间一块当模板，在普通截图
  `±40 px` 里搜一遍，最佳位移必须是 `(0,0)`（跨屏缩放取整时可能差 1 个物理
  像素），残差小就说明画面没串位。

别用「块比值定位洞的范围」：内容一动，块的均值就飘，洞的包围盒会糊成一大片。
只看整体均值（压暗对不对）加固定位置的残差（内容对不对）就够了。

## 满屏拖动的基准

`FOCUSSNAPS_BENCH=n:drag:8` 现在是**拖着整个选区沿对角线满屏跑**（每帧 24 逻辑
像素，碰到布局边界反弹），走的是真实的 `on_drag_begin` / `queue_pointer` 路径，
不是直接改 `doc.sel`。这样「选区离屏幕原点很远」的情况每帧都被覆盖到。

配合 `FOCUSSNAPS_TRACE=1`，跑完应当只有一条启动期的「重绘间隔」，
不该出现「洞的布局还没落地」——出现就说明我们设的位置和实际分配差了帧，
画面会和边框错开一格。

每帧 CPU 用 `os.wait4` 拿 `ru_utime + ru_stime`，跑两个不同帧数相减再平均；
系统噪声大的时候改成跑 2000 帧直接除，取多次里的最小值：

```python
# 满屏拖动，2000 帧
c.run(bin, 2000, "1300,300,600,400") / 2000
```

实测（DP-1）：亮洞从 cairo 贴 ImageSurface 改成纹理节点后，
固定选区 1.07 → 0.79 ms/帧，满屏拖动 1.03 → 0.72 ms/帧，帧率都还在 120 fps 上限。

## 「浮层刚出来就拖框会卡，甚至整个拖动被吞掉」

真机反馈：按热键调出浮层之后立刻开始拖框，拖动会卡，快速操作时那一下干脆
没反应。**纹理下载上线之后才出现**，改回后台读像素就没了——所以根因是主线程
被占住那几十毫秒。

链路上唯一的大活是「取像素」：`TextureDownloader` 一次 44 MB，DP-1 上 40~70 ms，
而且只能在主线程做（GL 上下文）；紧接着还要扫一遍像素认窗口，又是 20 ms。
这两下正好砸在用户按下鼠标、开始拖框的那一刻。现在全挪到后台线程了：

- 零拷贝抓屏拿到的是 dma-buf，**后台线程直接 mmap 读**（write-combined，44 MB
  要 1 秒左右），读完顺手把窗口识别的活也干完，再用 channel 把结果送回主线程。
  显示用的是纹理，像素晚到一秒完全不影响画面和拖动。
- 用户按了复制/保存就是在等结果，这时候没必要再等他那一秒：`hurry_pixels`
  用 GPU 侧下载（30 ms）抢一份出来。他此刻不会在拖框，卡一下看不见。
- 共享内存那条路像素本来就在手边，只把识别丢后台。

两个注意点：

- **`try_borrow_mut` 失败的帧绝不能让 `pointer_queued` 留在 `true`**：留着的话
  之后所有指针事件都不会再排，拖动彻底不动（这就是「操作被吃掉」的样子）。
  失败时必须返回 `ControlFlow::Continue`，下一帧再来。
- `FOCUSSNAPS_TESTSAVE="@"`（走真实保存路径那一档）现在**不等画面就绪**，
  按下去就调 `do_save`，应用自己该把请求挂住——这样才能测到上面这条链路。

### 怎么确认主循环有没有被自己卡住

`FOCUSSNAPS_TRACE=1` 现在会挂一个 5 ms 的探针，启动后两秒内任何超过 20 ms 的
间隔都会打一条「主循环被卡了 N ms」；拖动开始/结束也会打「按下」「松手」。
对着时间线看就知道卡在哪一段。

`ydotool` 别用：这台机器上 `ydotoold` 没在跑时命令会静默失败，
`mousedown`/`mouseup` 也不是它的子命令（只有 `click`），
拿它测「输入有没有丢」会得出完全错误的结论。
