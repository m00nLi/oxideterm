#!/bin/bash
# 编译 SSE2 shim 库（仅首次或 clang 版本变化时需要重新编译）
#
# 背景：clang-cl 不会内联 xwin CRT 头文件（emmintrin.h）中的 SSE2 intrinsics
# （_mm_loadu_si128 等），导致 zstd-sys 链接时出现 undefined symbol。
# 这个 shim 用纯 C 实现这 5 个函数，通过静态库链接补全符号。
#
# 用法：
#   ./tools/win-cross/build-sse2-shim.sh
#
# 产物：
#   tools/win-cross/sse2_shim.lib

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

echo "=== Compiling SSE2 shim ==="
clang --target=x86_64-pc-windows-msvc -O2 -c sse2_shim.c -o sse2_shim.o
llvm-ar crs sse2_shim.lib sse2_shim.o

echo "=== Done ==="
ls -la sse2_shim.lib
echo ""
echo "Symbols:"
llvm-nm sse2_shim.lib 2>/dev/null | grep " T "
