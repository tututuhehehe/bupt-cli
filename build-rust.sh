#!/bin/sh
# build-rust.sh —— 构建 bupt 的 macOS Apple Silicon 发布物
#
# 产物：dist/bupt-macos-arm64.zip（内含单文件 bupt，ARM64）
# 发布构建带 BUPT_CLI_RELEASE_BUILD=1 标记：只有这个标记 + release profile
# 的二进制才允许 `bupt update` 执行原位替换；开发构建一律只读检查。
#
# 用法：
#   ./build-rust.sh          构建并冒烟测试后打出 zip
#   ./build-rust.sh --keep   保留解包目录 dist/bupt-macos-arm64/
#
# 冒烟测试（全部离线、不碰真实凭据/会话，只隔离 HOME）：
#   ./bupt --version / help / status / uninstall --dry-run
# 任一步失败即中止，不会产出 zip。
set -e

SELF_DIR=$(cd -- "$(dirname "$0")" && pwd)
cd "$SELF_DIR"

VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/version *= *"\(.*\)"/\1/')
ASSET="bupt-macos-arm64"
TARGET_DIR="target/aarch64-apple-darwin/release"
STAGE="dist/$ASSET"

echo "==> 构建 bupt $VERSION (aarch64-apple-darwin, release, 发布标记)"
BUPT_CLI_RELEASE_BUILD=1 cargo build --locked --release --target aarch64-apple-darwin --bin bupt

BIN="$TARGET_DIR/bupt"
file "$BIN" | grep -q 'Mach-O 64-bit executable arm64' || {
  echo "错误：产物不是 macOS ARM64 可执行文件" >&2; exit 1;
}

echo "==> 离线冒烟测试（隔离 HOME，不读真实凭据）"
SMOKE=$(mktemp -d)
KEEP_STAGE=0
[ "${1:-}" = "--keep" ] && KEEP_STAGE=1
cleanup() {
  rm -rf "$SMOKE"
  [ "$KEEP_STAGE" = 1 ] || rm -rf "$STAGE"
}
trap cleanup EXIT
mkdir -p "$SMOKE/config" "$SMOKE/cache"
run_smoke() {
  HOME="$SMOKE" BUPT_CLI_CONFIG_DIR="$SMOKE/config" BUPT_CLI_CACHE_DIR="$SMOKE/cache" \
    "$BIN" "$@" >/dev/null
}
run_smoke --version
run_smoke help
run_smoke status
run_smoke uninstall --dry-run
echo "    冒烟通过"

echo "==> 打包 $ASSET.zip"
# Remove any previous artifact first: a failed or interrupted zip must never
# leave an old bundle looking like a fresh product.
rm -f "dist/$ASSET.zip" "dist/$ASSET.zip.sha256"
rm -rf "$STAGE"
mkdir -p "$STAGE"
install -m 700 "$BIN" "$STAGE/bupt"
(cd dist && zip -q -X "$ASSET.zip" "$ASSET/bupt")
shasum -a 256 "dist/$ASSET.zip" > "dist/$ASSET.zip.sha256"
cat "dist/$ASSET.zip.sha256"

[ "${1:-}" = "--keep" ] || rm -rf "$STAGE"
echo "==> 完成：dist/$ASSET.zip（$(du -h "dist/$ASSET.zip" | cut -f1)）"
