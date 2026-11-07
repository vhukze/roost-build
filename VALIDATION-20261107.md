
# Roost 阶段验收记录 — 2026-11-07

时间使用 Asia/Shanghai。本记录区分独立 Android 测试实例的实测结果、Tauri 应用源码完成情况和仍待验收的环节。

## 结论

**Magisk → LSPosed 完整激活 → 第三方模块真实 Hook 的基线链路已实测通过。** Root 原版来回以及 LSPosed 禁用后重装恢复也通过。Tauri 应用本轮功能已写入源码并完成语法/配置静态检查，尚未完成 GitHub 构建和编译后产品集成测试；本记录不是整套产品或完全替代 MuMu 的验收。

## 实测环境

- Windows 11；Google Android Emulator 37.3.7 / AEHD 2.2。
- 唯一操作目标：`emulator-5554`，AVD `roost-x86`。物理 OnePlus 6 未操作。
- Android 11 / API 30 Google APIs x86_64，ARM64/ARM32 native bridge。
- Magisk 28.0；LSPosed 1.9.2 (7024) Zygisk；SELinux Enforcing。
- 每次设备命令显式指定 `-s emulator-5554`；没有全局 ADB kill-server。

## 已通过的运行验证

| 检查 | 实际结果 | 证据 |
|---|---|---|
| 普通 ADB Shell → su | `uid=2000(shell)` → `uid=0(root)`，重启后保持 | `C:\RoostTest\evidence-20261107\shell-after-restart.txt`、`su-after-restart.txt`、`final-runtime.json` |
| 框架激活 | 管理器显示 Activated，SELinux Enforcing | `manager-final-status.xml`、`lsp-restored.xml` |
| 真实模块装载 | DisableFlagSecure v3.1.0 的类注入到 `com.lemon.lv` | `lspd-verbose.log` 第53–54行、`lspd-modules.log` |
| Hook 开启对照 | 模块仅作用于剪映；其非系统应用分支主动结束 Activity | `events.log` 第908、1028行，含 `wm_finish_activity ... MainActivity,app-request` |
| Hook 关闭对照 | 禁用测试模块后，剪映 MainActivity 保持前台，已有正常应用画面 | `lemon-hook-off.png`；`final-runtime.json` 与 `final-activities.txt` |
| Magisk → 原版 ramdisk | 无 magiskd/lspd/su 路径；剪映安装仍保留 | `switch-results.json`，2026-11-07 00:26:07–00:27:24 |
| 原版 → Magisk ramdisk | su、Magisk版本、Enforcing、lspd、管理器Activated恢复 | 同上 |
| LSPosed 禁用 → 重装 → 重启 | 禁用后lspd消失但su仍可用；官方安装器移除disable并暂存模块；重启后Activated | `lsp-switch-results.json`、`lsp-reinstall.log`，00:35:54–00:37:20 |

以上证据文件均位于 `C:\RoostTest\evidence-20261107\`（首行两项为同目录文件）。原版回切检查证明安装包及同一数据盘仍在，不等于已验证所有用户项目内容。最后剪映快照为启动页；完整导入、剪辑、音频和导出流程尚未验收。

测试模块当前保持禁用；未给系统框架添加测试作用域。框架本体保持启用。Magisk只在测试实例明确授予Shell UID2000；应用源码不自动改授权策略。

## 固定基线 SHA256

| 文件 | SHA256 |
|---|---|
| 原版 ramdisk.img | `81969123cbae196521fdaa36a606539e16a4144d6a8465bdd23d8d7b0c83d9c9` |
| ramdisk-magisk-preinit.img | `6f240d11e2cd858a224dae14d3c7ca28b5198a7f38f8472db373e5672260a874` |
| Magisk APK | `1b1eebac29f8ab1a41e5f20bbdceefb3341e93bc3d55a0f995c902b0fe877fe2` |
| LSPosed-v1.9.2-7024-zygisk-release.zip | `0ebc6bcb465d1c4b44b7220ab5f0252e6b4eb7fe43da74650476d2798bb29622` |
| DisableFlagSecure v3.1.0 APK | `ccd62abdbc6a7b2ebf512dc06e01c0789a0efc23ae4feb056e7a13cbdcdf4a64` |

准备ramdisk使用官方Magisk cpio备份流程，保留 `.backup/init.xz`、KEEPVERITY、KEEPFORCEENCRYPT，并设置 `PREINITDEVICE=metadata`。仅启动magiskd/lspd不是完整激活证据。

## 应用源码交付

- Android Emulator 后端：SDK/AVD/镜像/端口可配置，每VM独立AVD元数据、userdata/cache、boot和日志；旧缺backend配置保留legacy-qemu。
- 设备操作固定到已启动VM的配置快照和serial；每VM操作串行，ADB和加速检测设置超时。
- Windows启动时先CREATE_SUSPENDED，将原始Child进程句柄绑定每VM无名Job后才恢复初始线程；保留Job覆盖后代进程。停止先用精确SDK/serial的 `emu kill`，ADB未注册/卡住时只终止本次启动的专属Job，等待ActiveProcesses清零并核对端口；失败保留所有权，不按进程名或单独PID杀。
- 独立Win32 Python ctypes假进程fixture实跑22项检查通过：launcher退出后后代仍归同Job；终止Job后后代退出且Job为空，旁路未受管进程继续心跳并最终协作退出。证据：`C:\RoostTest\job-fixture-20261107\result.json`；仅验证Win32方案，不代表Rust编译或真实Emulator集成通过。
- Magisk运行资源部署，APK与Magisk ZIP安装分流；ZIP走push/`magisk --install-module`、检查结果、boot_id重启追踪。
- Root/LSP期望与实际分离；Root或模块核验失败清旧成功状态；LSP运行状态不冒充完整激活。
- LSPosed variant按具体module id/目录和版本核验，避免Riru被误当Zygisk。
- 每VM prepared ramdisk历史索引；SHA256与镜像身份匹配后回用；版本下拉合并已准备版本。不同Magisk版本 A→B→A 的产品UI流程待编译后实测。
- 模块查询按仓库隔离并保存导航状态；按实际kind安装，安装后刷新VM pending状态。
- 应用页接VM包列表、APK安装与确认后卸载；诊断页接现有定向诊断API，显示报告/错误。
- 运行中禁止修改CPU/内存等启动配置；核验保留资源差异的重启标记。
- 原 `artifacts.rs` 的FNV伪SHA256替换为sha2真实SHA256，并在活动内核写入前检查清单摘要；新增标准向量单测源码，未本地执行。

原始源码备份：`C:\Users\vhukze\Documents\dsh\虚拟机\Roost-source-backup-20261106-235928\app`；README旧版另有备份。

## 已执行的静态检查

- 10个Rust文件（含新增owned_process.rs）`rustfmt --emit stdout` 语法解析，输出丢弃，不修改源文件、不编译。
- 3个TypeScript文件 `createSourceFile` 语法解析，无emit、无语义类型检查。
- Cargo TOML/lock、package.json、Tauri JSON真解析；命令注册与invoke对照。
- `.github/workflows/build-app.yml` 使用现成js-yaml真解析通过，只有workflow_dispatch入口。
- 检查记录：`rust-static-checks.txt`、`frontend-ci-static-checks.txt`。

## GitHub 构建交接

已新增 `.github/workflows/build-app.yml`：Windows runner上执行锁定依赖安装、前端构建/类型检查、`cargo check/test --locked`、NSIS打包，上传安装器及源revision/摘要记录；不自动发布Release。

2026-11-07 01:10（Asia/Shanghai）补充：

- 新增真实Rust/Tokio进程树回归源码 `owned_process::tests::job_tree_survives_launcher_exit`：用当前测试exe精确运行ignored worker，不启动SDK/ADB/Emulator。专属Job继承后代、launcher退出后仍有ActiveProcesses、终止一个Job不影响另一个Job、心跳及有界清理均有断言。单测源码仅解析，尚待云端执行。
- 新增7项prepared boot隔离fixture单测：A→B→A私有缓存复用、原版ramdisk/build.prop指纹变化、缓存篡改且不转外部输入、跨VM隔离、错误root/key/变体/损坏JSON过滤、未准备变体失败、legacy记录回用升级。fixture中的A/B只是测试字节与版本标签，不是两个真实Magisk版本运行证据。
- 产品读取prepared manifest时补充显式key一致性检查；缺失key的旧记录仍兼容。路径抽到私有目录helper以隔离测试，生产入口仍使用当前VM目录。
- 工作流增至12步：测试输出保存为 `rust-tests.log`，失败时也通过 `always()` 上传日志和fixture文件；测试临时目录限定runner temp。依然仅手动触发。
- 已用 `gh repo view` 和 `gh workflow list --all` 只读核对 `vhukze/roost-build`：默认分支main，只有旧的 `build-kernel.yml` 与 `publish-manifest.yml`，未注册本轮 `build-app.yml`。当前源码位于独立DroidBox目录，并非该Git工作树。
- **云端验证的具体阻塞条件：目标仓库尚未包含本轮源码和新工作流；在不自行提交推送的约束下，本轮不发布、不触发旧源码构建充数。** 需要将审查后的本轮源码发布到构建分支，再dispatch新工作流。目标继续保持未完成状态。

**本轮没有本地编译，没有git commit/push，没有上传本轮源码，也没有触发旧源码的云构建。** 因此现在没有本轮云构建通过的结论或新安装器。需要把审查后的这一版源码发布到目标GitHub构建分支，再运行该工作流并用产物做产品验收；不能用旧工作流产物代替。

## 第二轮状态收敛修正 — 2026-11-07 01:17 +08:00

- 修正 `verify_vm_runtime`：Root核验失败即保持pending；即便desired/running配置相同、LSPosed为none，也不清除失败的Root状态。`root_applied`必须与目标版本一致。
- 修正 `verify_vm`：Magisk/lspd探测现在要求成功退出且仅含精确PRESENT/ABSENT标记；ADB离线、缺命令、缺标记或歧义输出均属于观测失败，不再把非零退出等同于进程缺席或Root已关闭。
- 新增5项Rust纯状态单测源码：Root失败保持pending、none选择也需确认、原资源/版本/模块重启差异保留、精确进程标记、失败命令不证明缺席。只解析，尚未编译/执行。
- 对 `emulator-5554` 执行源码中同一guest探测命令，Magisk/lspd均返回PRESENT；shell局部函数模拟正常缺席返回ABSENT，局部PATH缺命令返回127并归Unknown。四项符合预期；普通Shell仍UID2000，明确su为UID0。只读，没有重启或修改设备配置。
- 原始输出：`C:\RoostTest\evidence-20261107\goal-round2-process-probes.json`。这一结果只证明guest探测命令的区分行为，不是Rust编译或产品UI验收。
- 云端门槛未变：本轮源码尚未发布到目标仓库，禁止自行提交推送的约束保留。目标继续active，未声明全部完成。

## 仍待验收

1. GitHub端Rust类型/借用/Send约束、TS语义检查、单元测试、Windows打包。
2. 新安装器实际启动、每VM独立数据、启停所有权/端口释放、应用和诊断页面真实操作。
3. 多个真实Magisk版本的prepared映射来回及镜像改变/摘要损坏拒用测试。
4. KernelSU/APatch/SukiSU的匹配内核方案；当前镜像不将它们标为已支持。
5. 自动ramdisk准备、不同版本ABI资源兼容、完整剪映媒体链路/GPU与性能、多VM压力测试。
6. 保留MuMu作为回退；完成上述验收前不据此要求卸载MuMu。

## 本轮恢复状态

测试实例已恢复Magisk ramdisk、LSPosed Activated及Enforcing；2026-11-07 00:47:16快照确认非root ADB shell、su root、lspd存在以及剪映MainActivity仍在前台。未修改主机BCD/BIOS/Hyper-V/WHPX/testsign或其他游戏驱动，未重启主机，未删除原版ramdisk或应用数据。