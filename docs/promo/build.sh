#!/bin/bash
# 生成宣传图：拼字排版 + 裁切演示截图，输出两个尺寸
#
#   ./build.sh            用仓库里的 shot.png 重新拼图（只需要 rsvg-convert）
#   ./build.sh --capture  先重截演示画面再拼图（需要 mpv + 已安装的 FocusSnapS）
set -e
cd "$(dirname "$0")"

if [ "${1:-}" = "--capture" ]; then
  # 演示用的假桌面：两张窗口的示意图，不含任何真实屏幕内容
  rsvg-convert -w 3840 -h 1600 mock-desktop.svg -o mock-desktop.png
  # 全屏显示它，再让 FocusSnapS 在上面对它开浮层、画一套示例标注
  mpv --fs --no-osc --really-quiet --image-display-duration=inf --no-config \
    ./mock-desktop.png >/dev/null 2>&1 &
  MPV=$!
  sleep 2
  FOCUSSNAPS_TEST_SELECT=190,170,1000,520 \
  FOCUSSNAPS_TEST_ANNOTATE=promo \
  FOCUSSNAPS_TEST_PALETTE=1 \
    FocusSnapS -o "$(niri msg --json focused-output | sed 's/.*"name":"\([^"]*\)".*/\1/')" &
  APP=$!
  sleep 2.2
  FocusSnapS grab -o "$(niri msg --json focused-output | sed 's/.*"name":"\([^"]*\)".*/\1/')" --png shot.png >/dev/null
  kill "$APP" 2>/dev/null || true
  kill "$MPV" 2>/dev/null || true
  wait 2>/dev/null || true
fi

# 裁掉屏幕边缘，只留浮层和两个窗口
magick shot.png -crop 1580x1065+200+175 +repage shot-crop.png
cp shot-crop.png shot-tight.png

rsvg-convert -w 1600 -h 1250 promo.svg -o focus-snaps-promo.png
rsvg-convert -w 1280 -h 640  promo-wide.svg -o focus-snaps-promo-1280x640.png

rm -f shot-crop.png shot-tight.png mock-desktop.png
echo "已生成 focus-snaps-promo.png 与 focus-snaps-promo-1280x640.png"
