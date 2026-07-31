# OxideTerm Windows 编译指南

## 方案：GitHub Actions 原生编译

OxideTerm 的 Windows 二进制通过 GitHub Actions 在 `windows-latest` runner 上原生编译。

**原因**：GPUI 的 Windows 渲染器使用 Direct3D 11，构建时需要 `fxc.exe`（HLSL 着色器编译器）编译着色器字节码。`fxc.exe` 只存在于 Windows SDK 中，无法在 Linux 上交叉编译。

尝试过启用 wgpu 渲染器替代 DirectX 来绕过 `fxc.exe` 依赖，但在 Windows 上运行时黑屏，因此回退。

## 触发构建

### 自动构建（标签触发）

推送 `v*`、`native-v*` 或 `gpui-v*` 标签会自动触发全平台构建：

```bash
git tag v2.0.11
git push origin v2.0.11
```

### 手动构建

在 GitHub 仓库 → Actions → Native Package → Run workflow

## 已保留的交叉编译适配

## 交叉编译工具（保留但不推荐）

`tools/win-cross/` 目录下保留了交叉编译工具脚本，但**不推荐使用**，因为生成的 Windows 二进制会黑屏（缺少 DirectX 着色器）。

| 文件 | 用途 |
|------|------|
| `tools/win-cross/sse2_shim.c` | SSE2 intrinsics 补丁源码 |
| `tools/win-cross/sse2_shim.lib` | SSE2 shim 静态库 |
| `tools/win-cross/build-sse2-shim.sh` | 编译 SSE2 shim 的脚本 |
| `tools/win-cross/build-windows.sh` | Windows 交叉编译脚本（会黑屏） |
