#!/bin/bash
# OxideTerm Windows 交叉编译脚本（x86_64-pc-windows-msvc）
# 使用 clang-cl + lld-link + xwin CRT/SDK（cargo-xwin 内置缓存）
#
# 前置条件：
#   - cargo-xwin 已安装，xwin CRT/SDK 缓存在 ~/.cache/cargo-xwin/xwin/
#   - clang-cl, lld-link, llvm-lib 已安装
#   - tools/win-cross/sse2_shim.lib 已编译（见 build-sse2-shim.sh）
#   - .cargo/config.toml 已配置 Windows target linker
#
# 用法：
#   ./tools/win-cross/build-windows.sh
#
# 产物：
#   target/x86_64-pc-windows-msvc/release/oxideterm-native.exe

set -e

PROJECT_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$PROJECT_DIR"

# xwin CRT/SDK 路径
XWIN_CACHE="${HOME}/.cache/cargo-xwin/xwin"
CRT_INC="${XWIN_CACHE}/crt/include"
SDK_UCRT_INC="${XWIN_CACHE}/sdk/include/ucrt"
SDK_UM_INC="${XWIN_CACHE}/sdk/include/um"
SDK_SHARED_INC="${XWIN_CACHE}/sdk/include/shared"

# 编译器配置
export CC_x86_64_pc_windows_msvc=clang-cl
export CXX_x86_64_pc_windows_msvc=clang-cl
export AR_x86_64_pc_windows_msvc=llvm-lib
export CFLAGS_x86_64_pc_windows_msvc="--target=x86_64-pc-windows-msvc -I${CRT_INC} -I${SDK_UCRT_INC} -I${SDK_UM_INC} -I${SDK_SHARED_INC}"
export CXXFLAGS_x86_64_pc_windows_msvc="--target=x86_64-pc-windows-msvc -I${CRT_INC} -I${SDK_UCRT_INC} -I${SDK_UM_INC} -I${SDK_SHARED_INC}"

echo "=== Building Windows binary (x86_64-pc-windows-msvc) ==="
cargo build --release \
  --target x86_64-pc-windows-msvc

echo ""
echo "=== Build complete ==="
ls -lh target/x86_64-pc-windows-msvc/release/oxideterm-native.exe
