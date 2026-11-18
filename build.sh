#!/bin/sh
# build.sh —— 把 bupt 打包成单文件可执行二进制，并直接装到项目根目录 ./bupt
#
# 产物：./bupt（约 16MB，只依赖系统 libSystem/libz，不需要 Python）
# 因为 ~/.zshrc / ~/.zshenv 把 moon_cli_or_tool/* 全部加进 PATH，
# 装到根目录后 `bupt` 命令直接就是二进制，不再经过 shell 启动脚本。
#
# 源码直跑（开发用，需要 Python + .deps）请用 ./bupt-dev。
#
# 用法：
#   ./build.sh           正常构建并安装到 ./bupt
#   ./build.sh --clean   先删除 build/、dist/、bupt.spec（保留已装好的 ./bupt）
#
# 构建用 uvx 拉一个临时环境（pyinstaller + httpx + lxml），
# 不往项目 .deps 或系统 Python 里装任何东西。
#
# 安全：先打到 build/dist/ 里冒烟测试，只有测试通过才会覆盖 ./bupt，
# 中途失败不会把可用的旧二进制弄坏。
set -e

SELF_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
cd "$SELF_DIR"

PY_VER=3.14
NAME=bupt
TARGET="$SELF_DIR/$NAME"      # 最终安装位置：<项目根>/bupt
STAGE="build/dist"            # 临时产物目录，不放 dist/，避免与旧习惯冲突

if [ "${1:-}" = "--clean" ]; then
  rm -rf build dist "$NAME.spec"
  echo "已清理 build/ dist/ ${NAME}.spec（保留 ${TARGET}）"
fi

echo "==> 检查 .deps 依赖"
if [ ! -d ".deps/httpx" ] || [ ! -d ".deps/lxml" ]; then
  echo "错误：缺少 .deps（httpx / lxml）。先运行：" >&2
  echo "  uv pip install --target .deps 'httpx>=0.27.0' 'lxml>=5.0.0'" >&2
  exit 1
fi

echo "==> 生成入口脚本"
STUB_DIR=$(mktemp -d)
STUB="$STUB_DIR/entry.py"
trap 'rm -rf "$STUB_DIR"' EXIT
cat > "$STUB" <<'PY'
import sys

from bupt_cli.cli import main

if __name__ == "__main__":
    raise SystemExit(main())
PY

echo "==> PyInstaller 打包（Python ${PY_VER}，可能需 1-2 分钟）"
rm -rf "$STAGE"
uvx --python "$PY_VER" \
  --with httpx --with lxml --with pyinstaller \
  pyinstaller \
    --onefile \
    --name "$NAME" \
    --paths "$SELF_DIR/src" \
    --collect-submodules bupt_cli \
    --hidden-import lxml.etree \
    --hidden-import lxml._elementpath \
    --exclude-module tkinter \
    --exclude-module pytest \
    --exclude-module unittest \
    --exclude-module pydoc \
    --noconfirm \
    --log-level WARN \
    --distpath "$STAGE" \
    "$STUB"

NEW="$SELF_DIR/$STAGE/$NAME"
if [ ! -x "$NEW" ]; then
  echo "错误：打包失败，$STAGE/$NAME 不存在" >&2
  exit 1
fi

echo
echo "==> 冒烟测试（清空 PYTHONPATH 验证独立性）"
env -u PYTHONPATH "$NEW" help >/dev/null 2>&1 \
  || { echo "  失败：新二进制无法独立运行，未覆盖 $TARGET" >&2; exit 1; }
env -u PYTHONPATH "$NEW" --version >/dev/null 2>&1 \
  || { echo "  失败：--version 异常，未覆盖 $TARGET" >&2; exit 1; }
echo "  独立运行 OK"

echo
echo "==> 安装到 ${TARGET}（原子替换）"
chmod 755 "$NEW"
mv "$NEW" "$TARGET.new"
mv "$TARGET.new" "$TARGET"
rmdir "$STAGE" 2>/dev/null || true

echo "==> 清理中间产物"
rm -rf build "$NAME.spec"

echo
echo "==> 产物"
ls -lh "$TARGET"
PATH_PICK=$(command -v "$NAME" 2>/dev/null || true)
echo "  PATH 上的 $NAME -> ${PATH_PICK:-（不在 PATH，重开一个 shell 试试）}"

echo
echo "完成。试用："
echo "  $NAME status --pretty"
echo
echo "改源码后想立刻验证、不打包：./bupt-dev <args>"
echo "配置与会话统一存放在 ~/.bupt-cli（首次使用先运行 \`bupt login\`）"
