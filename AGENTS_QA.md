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
