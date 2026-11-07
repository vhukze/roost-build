# Roost

Windows 原生 Android 虚拟机管理器 —— 图形化管理、停机切换匹配的 Root 启动镜像、不依赖 WSL2。当前 Tauri 应用状态与验收边界见下节。

为 **LSPosed 模块开发** 场景设计。

---


## Tauri App 实现交接（2026-11-07）

**当前 app 源码尚未编译，尚未完成 app 集成验收。** Rust/TypeScript 语法解析、Cargo TOML/lock 解析及命令注册对照已通过；这些静态检查不等于类型检查、云构建通过或产品启动验收。后续构建统一通过 GitHub Actions。

- 新建 VM 默认 Android Emulator / AEHD 检测；缺少 backend 字段的旧配置保留 legacy-qemu。SDK、源 AVD、完整 x86_64 镜像目录和端口须显式配置；不安装或切换宿主驱动。
- 当前 Root 范围仅 none / Magisk。KernelSU、APatch、其他依赖内核的方案未支持；历史 Python/Overlay 文档不代表本次 app 功能。
- Magisk 首次使用对应 ABI/版本的 prepared ramdisk，配置 prepared_root 与 prepared_ramdisk；已准备历史版本按每VM索引、SHA256与镜像身份校验后回用。下载 APK 不代表可启动。准备文件须保留官方 cpio backup（.backup/init.xz）、KEEPVERITY、KEEPFORCEENCRYPT 与正确 PREINITDEVICE；App 不自动证明 cpio 内容正确。
- Windows 启动进程先挂起、绑定每VM专属 Job 再恢复；ADB 尚未注册或停止失败时有专属进程树兜底，不按进程名终止。独立 Win32 fixture 22项通过，Rust 编译及真实 Emulator 产品集成仍待验收。
- 应用管理与诊断页已接 VM 定向 API；模块搜索按仓库保留状态，安装后刷新待重启状态。源码完成不等于编译后验收通过。
- 完整证据、摘要与云构建交接见 [阶段验收记录](VALIDATION-20261107.md)。新增 build-app.yml 仅手动触发；本轮未 commit/push、未上传新源码、未执行本地编译。
- 每 VM 独立 boot、AVD 元数据、userdata/cache 和启动日志；不将测试实例的 userdata 作为产品默认。首次需在 Magisk 管理器明确授权 Shell；初始化操作不自动修改授权策略或执行 adb root。
- APK 走 APK 安装；Magisk ZIP 走精确目标 VM 的 push 与 magisk --install-module，并检查模块目录及退出状态。重启需求按 boot_id 跟踪。
- LSPosed 自动基线仅公开 1.9.2 Zygisk。已安装、进程运行、partial、running-unverified 与待重启分别表达；lspd 存在不代表完整激活，仍需管理器核验。其他分发包、许可证版本不承诺可用。
- 独立测试 VM 的 none → Magisk 恢复、普通 Shell 授权后的 su、LSPosed 管理器 Activated 与 Enforcing 已由测试负责人实测；这不代替本次 app 的集成验收。原版 ramdisk 启动后测试 app 数据保留。测试证据由测试负责人单独维护。

以上说明优先界定本次 app 交付；下文旧版热切换、免准备及各阶段完成描述属于历史 CLI/骨架说明，不构成本次 app 已验收承诺。

## 这是什么

一个跑在 Windows 用户态的可定制 Android 虚拟机管理器。

**核心能力**

- **Root 可插拔** —— KernelSU / Magisk / 自定义，一条命令或界面按钮切换
- **零系统侵入** —— 不装内核驱动、不改 BCD、不启用 Windows 功能
- **双击即用** —— 不需要 WSL2 / Docker / Hyper-V 管理器
- **与虚拟化破解环境共存** —— 纯 TCG 模式，不占用 VT-x
- **崩溃归因** —— 自动判断闪退属于翻译 / 图形 / ABI / 检测

---

## 快速开始

### 双击入口

```
Roost.cmd
```

交互式菜单，覆盖全部功能。

### 命令行入口

```bash
python roost.py status          # 环境与镜像状态
python roost.py vms             # 列出虚拟机
python roost.py start vm1       # 启动虚拟机
python roost.py overlays        # 列出 Root overlay
python roost.py switch vm1 ksu  # 切换 Root
python roost.py compat          # 兼容性矩阵
python roost.py gui             # 图形界面
```

---

## 当前状态

| 阶段 | 内容 | 状态 |
|---|---|---|
| **阶段 1** | 运行时、引擎、GUI、Overlay 骨架、文档 | ✅ 完成并实测 |
| **阶段 2** | AOSP arm64 镜像构建 | ⏸ 脚本就绪，待构建 |
| **阶段 3** | Root overlay 二进制填充与切换验证 | ⏸ 骨架就绪 |
| **阶段 4** | GUI 完整交互、单文件打包 | ⏸ 骨架就绪 |

### 已实测通过的组件

| 组件 | 版本 | 验证方式 |
|---|---|---|
| QEMU | 11.1.0 | ✅ TCG 实启动，5 秒消耗 6.59 秒 CPU |
| qemu-img | 11.1.0 | ✅ |
| adb | 1.0.41 | ✅ |
| scrcpy | 5.0.1 | ✅ |
| PySide6 | 6.12.0 | ✅ GUI 窗口实弹窗 1100×720 |
| Python 引擎 | 5 模块 | ✅ 全部语法通过 + 功能实测 |
| Root overlay | 4 套 | ✅ 结构校验通过 |

---

## 目录结构

```
DroidBox/
├── Roost.cmd              ← 双击入口
├── roost.ps1              ← 交互式菜单
├── roost.py                ← 统一 CLI
├── docs/
│   ├── ARCHITECTURE.md       完整架构设计
│   ├── BUILD-AOSP.md         AOSP 构建指南
│   ├── ROOT-OVERLAY.md       Overlay 制作规范
│   └── STAGE2-GUIDE.md       阶段 2 操作手册
├── setup/
│   ├── 01-install-qemu.ps1   QEMU 便携安装（解包模式）
│   ├── 02-install-runtime.ps1 adb / scrcpy
│   └── 03-check-env.ps1      环境自检（只读）
├── tools/
│   ├── build-overlay.ps1     Overlay 结构校验
│   └── qemu-launcher.ps1     QEMU 启动调试
├── runtime/                  绿色运行时
│   ├── qemu/                 QEMU 11.1.0
│   ├── platform-tools/       adb
│   └── scrcpy/               scrcpy 5.0.1
├── images/
│   ├── base/                 AOSP 镜像（待阶段 2）
│   └── overlays/
│       ├── none/             无 Root
│       ├── ksu/              KernelSU
│       ├── magisk/           Magisk
│       └── _template/        自定义模板
├── data/
│   ├── vms/vm1/              虚拟机配置
│   ├── compat/               兼容性矩阵
│   └── diagnostics/          崩溃诊断报告
├── gui/main_window.py        PySide6 界面
└── scripts/
    ├── core/
    │   ├── vm_controller.py  虚拟机生命周期
    │   ├── root_engine.py    Root 切换引擎
    │   ├── adb_bridge.py     adb 封装
    │   ├── image_store.py    镜像管理
    │   ├── crash_diagnose.py 崩溃归因
    │   └── compat_matrix.py  兼容性矩阵
    └── build/
        └── build-aosp.sh     AOSP 构建（Linux 侧）
```

---

## 技术路线

**跑 arm64 Android 镜像 + QEMU TCG 纯软件翻译。**

为什么不用 x86 镜像 + libndk_translation：

- libndk 闭源，出问题无法定位、无法修
- 需求是「arm 指令翻译全正常」，必须能改翻译层

为什么不用 WHPX 硬件加速：

- WHPX 占用 VT-x，与 Denuvo 破解 hypervisor 互斥
- TCG 在 Android 场景下可用，且对 LSP 开发足够

---

## 崩溃诊断

这是针对「ARM 应用闪退」的核心工具：

```bash
python scripts/core/crash_diagnose.py --package com.example.app
```

自动输出：

- **崩溃阶段**：启动即崩 / UI 后崩 / 登录后崩
- **归因**：指令翻译 / 图形 / ABI / 系统 API / 检测 / 权限 / OOM
- **置信度** 与**匹配证据**
- **针对性排查建议**

已用模拟日志验证 5 类归因全部正确命中。

---

## 与虚拟化破解环境的关系

| 维度 | 影响 |
|---|---|
| 系统状态（BCD / 驱动 / 功能） | ✅ 完全不变 |
| 破解 hypervisor 能否加载 | ✅ 不受影响（TCG 不碰 VT-x） |
| 开机 / 关机 | ✅ 不受影响 |
| **同时运行时的性能** | ⚠️ **会争抢 CPU** |

**唯一约束**：三者同跑会抢 CPU。玩游戏时关 Roost，开发时关游戏。

---

## 卸载

**删除 `Roost` 目录即可。**

没有注册表项、没有系统服务、没有 PATH 修改、没有启动配置变更。

---

## 已知限制

| 限制 | 原因 |
|---|---|
| 3D 游戏性能差 | AMD 显卡在 Windows 无 GPU 直通，图形走 SwiftShader |
| ARM app 慢 5~15x | 纯软件翻译的固有代价 |
| 首次启动需 1~3 分钟 | TCG 预热 + 镜像首读 |
| 无 Google 服务 | 使用 AOSP 原生镜像 |

**对 LSP 模块开发场景，以上限制均可接受。**
