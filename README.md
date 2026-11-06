# roost-build

Roost 的云端编译仓库。**所有构建都在 GitHub Actions 上完成，本地机器只下载产物。**

## 为什么放云端

| 项目 | 本地构建 | GitHub Actions |
|---|---|---|
| 磁盘占用 | 10~250 GB | 0（产物几百 MB） |
| 环境依赖 | 需装 clang / 交叉编译工具链 | 无需安装 |
| 对现有环境影响 | 可能干扰游戏/虚拟化环境 | 零影响 |
| 可复现性 | 取决于本机状态 | 完全可复现，日志全留 |

## 工作流

### build-kernel.yml — 编译 arm64 内核

**什么时候需要**

| 场景 | 是否需要编译内核 |
|---|---|
| 普通运行 Android + Magisk root | **不需要**，用 Google 预编译镜像自带内核 |
| 需要 KernelSU / APatch（内核态 root） | **需要** |

**参数**

| 参数 | 说明 |
|---|---|
| `kernel_branch` | `android-goldfish-4.14-gchips` （模拟器专用，推荐）|
| `defconfig` | `ranchu_defconfig` （QEMU virt 机器）|
| `enable_ksu` | 是否集成 KernelSU |
| `ksu_branch` | KernelSU 分支 |

**产物**

```
kernel           内核镜像
kernel.config    编译配置（排查用）
build-info.txt   构建信息
kernel.sha256    校验和
```

**已经预置的 QEMU 必需配置**

```
CONFIG_VIRTIO / VIRTIO_PCI / VIRTIO_BLK / VIRTIO_NET
CONFIG_DRM_VIRTIO_GPU      # 图形
CONFIG_SERIAL_AMBA_PL011   # 串口控制台 ttyAMA0
CONFIG_EXT4_FS             # 根文件系统
```

## 使用方法

### 触发编译

```bash
# 用 gh 命令行
gh workflow run build-kernel.yml \
  -f kernel_branch=android-goldfish-4.14-gchips \
  -f defconfig=ranchu_defconfig \
  -f enable_ksu=false

# 或去网页 Actions 页面点 Run workflow
```

### 下载产物

产物会自动创建 Release，也可用：

```bash
gh run list --workflow=build-kernel.yml --limit 1
gh run download <run-id>
```

### 替换到 Roost

把下载的 `kernel` 放到：

```
DroidBox/images/base/kernel
```

## 免费额度

- 公开仓库：**无限**
- 私有仓库：2000 分钟/月
- 一次内核编译约 **20~40 分钟**

## 注意

`android-goldfish` 分支版本较老（4.14），若需要新内核请改用 GKI 分支（`android13-5.15` / `android14-6.1`），但需要相应的 defconfig 与设备树支持。
