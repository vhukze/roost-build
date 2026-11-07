/**
 * Tauri 后端接口封装
 */

import { invoke } from "@tauri-apps/api/core";

// ------------------------------------------------------------------ 类型

export interface EnvStatus {
  emulator: boolean;
  acceleration_note: string;
  qemu: boolean;
  qemu_img: boolean;
  adb: boolean;
  scrcpy: boolean;
  images_ready: boolean;
  missing_images: string[];
  overlay_count: number;
  project_root: string;
}

export interface VmConfig {
  id: string;
  name: string;
  memory_mb: number;
  cores: number;
  base_image: string;
  root_manager: string;
  disk: string;
  adb_port: number;
  accel: string;
  tb_size_mb: number;
  gpu: string;
  created: string;
  notes: string;
  lsp_version?: string;
  backend: "legacy-qemu" | "android-emulator";
  sdk_path: string;
  avd_home: string;
  avd_name: string;
  emulator_image_dir: string;
  console_port: number;
  prepared_ramdisk: string;
  prepared_root: string;
  magisk_path: string;
  root_applied?: string;
  lsp_applied?: string;
  lsp_state: string;
  pending_restart: boolean;
  module_pending_boot_id?: string;
}

export interface OverlayInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  source: string;
  license: string;
  android_min: number;
  android_max: number;
  requires_kernel_patch: boolean;
  zygisk_provider: boolean;
  checks: { su_path?: string; verify_cmd?: string };
  symlinks: { path: string; target: string }[];
  has_binary: boolean;
  binary_count: number;
  total_bytes: number;
  zygisk: boolean;
}

export interface LaunchResult {
  ok: boolean;
  pid: number | null;
  message: string;
  command: string[];
  backend: string;
  serial: string;
}

export interface PreparedBoot { root: string; key: string; source: string; sha256: string; ramdisk: string }

export interface CmdResult { ok: boolean; stdout: string; stderr: string; code: number }
export interface ModuleInstallResult extends CmdResult { needs_reboot: boolean; module_id: string }
export interface VerifyVmResult { root_verified: boolean; lsp_verified: boolean; lsp_installed: boolean; lsp_state: string; pending_restart: boolean; message: string }

export interface DeviceInfo {
  connected: boolean;
  serial: string;
  model: string;
  android_version: string;
  sdk: string;
  abi: string;
  debuggable: string;
  root: boolean;
}

export interface SwitchResult {
  ok: boolean;
  from: string;
  to: string;
  message: string;
  rolled_back: boolean;
  steps: string[];
}

export interface DiagnoseReport {
  package: string;
  timestamp: string;
  phase: string;
  phase_label: string;
  cause: string;
  cause_label: string;
  confidence: number;
  evidence: string[];
  suggestions: string[];
  matched_signals: Record<string, string[]>;
  crash_log: string;
  tombstone: string;
}

export interface AppRecord {
  package: string;
  name: string;
  version: string;
  abi: string;
  state: string;
  root_mode: string;
  issue: string;
  workaround: string;
  notes: string;
  last_tested: string;
  test_count: number;
}

// ------------------------------------------------------------------ API

export const api = {
  // 环境
  envStatus: () => invoke<EnvStatus>("get_env_status"),
  projectRoot: () => invoke<string>("get_project_root"),

  // 虚拟机
  listVms: () => invoke<VmConfig[]>("list_vms"),
  listPreparedBoots: (vmId: string) => invoke<PreparedBoot[]>("list_prepared_boots", { vmId }),
  getVm: (vmId: string) => invoke<VmConfig>("get_vm", { vmId }),
  saveVm: (config: VmConfig) => invoke<void>("save_vm", { config }),
  deleteVm: (vmId: string) => invoke<void>("delete_vm", { vmId }),
  createVm: (vmId: string, name: string) => invoke<VmConfig>("create_vm", { vmId, name }),
  launchVm: (vmId: string, headless = false) =>
    invoke<LaunchResult>("launch_vm", { vmId, headless }),
  stopVm: (vmId: string) => invoke<string>("stop_vm", { vmId }),
  vmRunning: (vmId: string) => invoke<boolean>("vm_running", { vmId }),
  runningVms: () => invoke<string[]>("running_vms"),
  vmDiskInfo: (vmId: string) => invoke<number | null>("vm_disk_info", { vmId }),
  buildVmCommand: (vmId: string, headless = false) =>
    invoke<string[]>("build_vm_command", { vmId, headless }),

  // Overlay
  listOverlays: () => invoke<OverlayInfo[]>("list_overlays"),
  getOverlay: (overlayId: string) => invoke<OverlayInfo>("get_overlay", { overlayId }),

  // ADB
  adbConnect: (vmId: string) => invoke<boolean>("adb_connect", { vmId }),
  adbDeviceInfo: (vmId: string) => invoke<DeviceInfo>("adb_device_info", { vmId }),
  adbListPackages: (vmId: string) => invoke<string[]>("adb_list_packages", { vmId }),
  adbInstall: (vmId: string, apk: string) => invoke<CmdResult>("adb_install", { vmId, apk }),
  adbUninstall: (vmId: string, packageName: string) =>
    invoke<CmdResult>("adb_uninstall", { vmId, package: packageName }),
  adbLogcat: (vmId: string, lines = 300) => invoke<string>("adb_logcat", { vmId, lines }),
  adbCrashLog: (vmId: string, lines = 200) => invoke<string>("adb_crash_log", { vmId, lines }),

  // Root
  switchRoot: (vmId: string, target: string, previous: string) =>
    invoke<SwitchResult>("switch_root", { vmId, target, previous }),
  verifyRoot: (vmId: string, overlayId: string) =>
    invoke<{ ok: boolean; message: string }>("verify_root", { vmId, overlayId }),
  checkRoot: (vmId: string) => invoke<boolean>("check_root", { vmId }),

  initializeVmMagisk: (vmId: string) => invoke<CmdResult>("initialize_vm_magisk", { vmId }),
  installMagiskModule: (vmId: string, path: string) =>
    invoke<ModuleInstallResult>("install_magisk_module", { vmId, path }),
  applyVmLsposed: (vmId: string) => invoke<ModuleInstallResult>("apply_vm_lsposed", { vmId }),
  verifyVmRuntime: (vmId: string) => invoke<VerifyVmResult>("verify_vm_runtime", { vmId }),

  // 诊断
  diagnoseApp: (vmId: string, packageName: string, waitSecs = 30) =>
    invoke<DiagnoseReport>("diagnose_app", { vmId, package: packageName, waitSecs }),

  // 兼容性
  listCompat: () => invoke<AppRecord[]>("list_compat"),
  upsertCompat: (record: AppRecord) => invoke<void>("upsert_compat", { record }),
  setCompatState: (packageName: string, stateName: string, issue?: string) =>
    invoke<void>("set_compat_state", { package: packageName, stateName, issue }),
  removeCompat: (packageName: string) => invoke<boolean>("remove_compat", { package: packageName }),
  compatStats: () => invoke<Record<string, number>>("compat_stats"),
  exportCompatCsv: () => invoke<string>("export_compat_csv"),

  // Root 下载
  listRootManagers: () => invoke<RootManager[]>("list_root_managers"),
  downloadRoot: (
    manager: string,
    version: string,
    url: string,
    assetName: string,
    source: "direct" | "mirror"
  ) => invoke<string>("download_root", { manager, version, url, assetName, source }),
  removeRootVersion: (manager: string, version: string) =>
    invoke<void>("remove_root_version", { manager, version }),
  checkRootDownloaded: (manager: string, version: string) =>
    invoke<boolean>("check_root_downloaded", { manager, version }),
  searchRootVersions: (manager: string, query: string) =>
    invoke<string[]>("search_root_versions", { manager, query }),

  // LSPosed
  listLsposed: () => invoke<PkgManager>("list_lsposed"),
  downloadLsposed: (
    version: string,
    variant: string,
    url: string,
    assetName: string,
    useMirror: boolean
  ) =>
    invoke<string>("download_lsposed", { version, variant, url, assetName, useMirror }),
  removeLsposed: (version: string, variant: string) =>
    invoke<void>("remove_lsposed", { version, variant }),

  // 模块仓库
  searchModules: (org: "lsposed" | "magisk", query: string, page: number) =>
    invoke<ModuleInfo[]>("search_modules", { org, query, page }),
  moduleRelease: (fullName: string, kind: "apk" | "magisk_zip") =>
    invoke<ModuleInfo>("module_release", { fullName, kind }),
  downloadModule: (
    fullName: string,
    apkUrl: string,
    apkName: string,
    useMirror: boolean
  ) => invoke<string>("download_module", { fullName, apkUrl, apkName, useMirror }),

  // AOSP 镜像
  listAospImages: () => invoke<ImageEntry[]>("list_aosp_images"),
  downloadAospImage: (api: number, url: string) =>
    invoke<string>("download_aosp_image", { api, url }),
  removeAospImage: (api: number) => invoke<void>("remove_aosp_image", { api }),
};

// ------------------------------------------------------------------ LSP / 模块类型

export interface PkgVersion {
  version: string;
  variant: string;
  size: string;
  date: string;
  url: string;
  asset_name: string;
  downloaded: boolean;
  featured: boolean;
}

export interface PkgManager {
  id: string;
  name: string;
  desc: string;
  source: string;
  versions: PkgVersion[];
  status: string;
}

export interface ModuleInfo {
  kind: "apk" | "magisk_zip";
  full_name: string;
  package: string;
  name: string;
  description: string;
  stars: number;
  updated: string;
  html_url: string;
  apk_url: string;
  apk_name: string;
  version: string;
}

// ------------------------------------------------------------------ Root 类型

export interface RootVersion {
  version: string;
  size: string;
  date: string;
  downloaded: boolean;
  url: string;
  asset_name: string;
  prerelease: boolean;
  featured: boolean;
}

export interface RootManager {
  id: string;
  name: string;
  desc: string;
  source: string;
  license: string;
  kind: "ready" | "needs-kernel" | "manual";
  zygisk: boolean;
  versions: RootVersion[];
  status: string;
}

export interface DownloadProgress {
  manager: string;
  version: string;
  downloaded: number;
  total: number;
  percent: number;
  status: "downloading" | "extracting" | "done" | "error";
  message: string;
}

// ------------------------------------------------------------------ AOSP 镜像

export interface ImageEntry {
  api: number;
  tag: string;
  abi: string;
  size: string;
  url: string;
  downloaded: boolean;
  android_version: string;
  note: string;
}

export interface ImageProgress {
  api: number;
  downloaded: number;
  total: number;
  percent: number;
  stage: string;
  message: string;
}
