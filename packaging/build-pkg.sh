#!/bin/bash
# 打包并安装 FocusSnapS：在 packaging/ 里生成源码 tarball，再 makepkg -si
set -e
cd "$(dirname "$0")/.."
VER=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
NAME="focus-snaps-$VER"
TMP=$(mktemp -d)
mkdir -p "$TMP/$NAME"
mkdir -p "$TMP/$NAME/packaging"
rsync -a --exclude target --exclude .git \
  Cargo.toml Cargo.lock README.md LICENSE src "$TMP/$NAME/"
rsync -a packaging/focus-snaps.desktop "$TMP/$NAME/packaging/"
tar -C "$TMP" -czf "packaging/$NAME.tar.gz" "$NAME"
rm -rf "$TMP"
echo "已生成 packaging/$NAME.tar.gz"
cd packaging
makepkg -si --nocheck --noconfirm
