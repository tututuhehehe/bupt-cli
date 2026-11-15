#!/bin/sh
# build.sh —— 把 bupt 打包成单文件可执行二进制（macOS arm64）
#
# 产物：dist/bupt（约 16MB，只依赖系统 libSystem/libz）
# 不覆盖项目根目录的 bupt 启动脚本；两者行为一致。
#
# 用法：
#   ./build.sh           正常构建
#   ./build.sh --clean   先删除 build/ 与 dist/
#
# 构建用 uvx 拉一个临时环境（pyinstaller + httpx + lxml），
# 不往项目 .deps 或系统 Python 里装任何东西。
set -e

SELF_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
cd "$SELF_DIR"

PY_VER=3.14
NAME=bupt

if [ "${1:-}" = "--clean" ]; then
  rm -rf build dist "$NAME.spec"
  echo "已清理 build/ dist/"
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

echo "==> PyInstaller 打包（Python $PY_VER，可能需 1-2 分钟）"
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
    "$STUB"

if [ ! -x "dist/$NAME" ]; then
  echo "错误：打包失败，dist/$NAME 不存在" >&2
  exit 1
fi

echo
echo "==> 清理中间产物"
rm -rf build "$NAME.spec"

echo "==> 产物"
ls -lh "dist/$NAME"
echo
echo "==> 冒烟测试（清空 PYTHONPATH 验证独立性）"
env -u PYTHONPATH "dist/$NAME" help >/dev/null 2>&1 \
  && echo "  独立运行 OK" \
  || { echo "  失败：二进制无法独立运行" >&2; exit 1; }

echo
echo "完成。试用："
echo "  ./dist/$NAME status --pretty"
echo
echo "配置与会话统一存放在 ~/.bupt-cli（首次使用先运行 `bupt login`）"
