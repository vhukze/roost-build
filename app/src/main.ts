/**
 * Roost — 主应用
 */

import "./styles.css";
import { api, type VmConfig, type OverlayInfo, type EnvStatus, type RootManager, type RootVersion, type DownloadProgress } from "./api";
import { icon } from "./icons";
import { roostIcon, roostLogo, roostMark } from "./brand";
import { renderLspPage, renderMagiskModulesPage } from "./pages-modules";
import { renderImagesPage } from "./pages-images";
import { createGuestPreview } from "./guest-preview";

// ------------------------------------------------------------------ 类型

type PageId =
  | "vms"
  | "root"
  | "lsp"
  | "magisk-mod"
  | "images"
  | "apps"
  | "diagnose"
  | "compat"
  | "settings";

interface NavDef {
  id: PageId;
  label: string;
  icon: string;
  title: string;
  desc: string;
}

const NAV: NavDef[] = [
  { id: "vms", label: "虚拟机", icon: "monitor", title: "虚拟机", desc: "管理和运行 Android 虚拟机" },
  { id: "root", label: "Root 管理", icon: "shield", title: "Root 管理器", desc: "可插拔的 Root 授权层" },
  { id: "lsp", label: "LSP 管理", icon: "layers", title: "LSP 管理", desc: "LSPosed 版本与模块仓库" },
  { id: "magisk-mod", label: "Magisk 模块", icon: "package", title: "Magisk 模块", desc: "模块仓库与本地安装" },
  { id: "images", label: "系统镜像", icon: "image", title: "系统镜像", desc: "AOSP arm64 镜像下载" },
  { id: "apps", label: "应用管理", icon: "grid", title: "应用管理", desc: "安装与卸载应用" },
  { id: "diagnose", label: "崩溃诊断", icon: "activity", title: "崩溃诊断", desc: "定位闪退原因" },
  { id: "compat", label: "兼容性", icon: "list", title: "兼容性矩阵", desc: "应用兼容性记录" },
  { id: "settings", label: "设置", icon: "settings", title: "设置", desc: "运行时与偏好" },
];

/** 导航分组 */
const NAV_GROUPS: { label: string; ids: PageId[] }[] = [
  { label: "工作区", ids: ["vms", "images", "root", "apps"] },
  { label: "框架与模块", ids: ["lsp", "magisk-mod"] },
  { label: "诊断", ids: ["diagnose", "compat"] },
  { label: "系统", ids: ["settings"] },
];

interface LogEntry {
  time: string;
  text: string;
  level: "info" | "ok" | "warn" | "err";
}

const state = {
  page: "vms" as PageId,
  env: null as EnvStatus | null,
  vms: [] as VmConfig[],
  overlays: [] as OverlayInfo[],
  currentVmId: null as string | null,
  running: new Set<string>(),
  starting: new Set<string>(),
  bootSessions: new Map<string, symbol>(),
  headless: true,
  logs: [] as LogEntry[],
  managers: [] as RootManager[],
  managersLoading: false,
  lsp: null as import("./api").PkgManager | null,
  lspLoading: false,
  modules: [] as import("./api").ModuleInfo[],
  modulesLoading: false,
  moduleQuery: "",
  moduleOrg: "lsposed" as "lsposed" | "magisk",
  moduleResults: { lsposed: [], magisk: [] } as Record<"lsposed" | "magisk", import("./api").ModuleInfo[]>,
  moduleQueries: { lsposed: "", magisk: "" },
  moduleBusy: { lsposed: false, magisk: false },
  preparedBoots: {} as Record<string, import("./api").PreparedBoot[]>,
  preparedKeys: {} as Record<string, string>,
  aospImages: [] as import("./api").ImageEntry[],
  aospLoading: false,
  aospProgress: null as import("./api").ImageProgress | null,
};

// ------------------------------------------------------------------ 工具

const now = () =>
  new Date().toLocaleTimeString("zh-CN", { hour12: false });

function log(text: string, level: LogEntry["level"] = "info") {
  state.logs.push({ time: now(), text, level });
  if (state.logs.length > 500) state.logs.shift();
  paintConsole();
}

const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

function fmtBytes(n: number) {
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(1) + " GB";
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + " MB";
  if (n >= 1 << 10) return (n / (1 << 10)).toFixed(0) + " KB";
  return n + " B";
}

// ------------------------------------------------------------------ 骨架

function paintShell() {
  document.querySelector<HTMLDivElement>("#app")!.innerHTML = `
    <header class="titlebar">
      <div class="brand">${roostLogo({ height: 32, dark: true })}</div>
      <div class="divider"></div>
      <div class="page-name" id="page-name">虚拟机</div>
      <div class="spacer"></div>
      <div id="head-actions" class="row" style="gap:8px"></div>
    </header>
    <div class="body">
      <nav class="rail" id="rail"></nav>
      <main class="view" id="view"></main>
    </div>
  `;
}

function paintRail() {
  const rail = document.querySelector<HTMLElement>("#rail")!;

  const groups = NAV_GROUPS.map((g) => {
    const items = g.ids
      .map((id) => NAV.find((n) => n.id === id)!)
      .map(
        (n) => `
        <button class="rail-item ${n.id === state.page ? "active" : ""}"
                data-page="${n.id}" title="${n.desc}">
          ${icon(n.icon as any, 17)}
          <span class="lbl">${n.label}</span>
        </button>`
      )
      .join("");
    return `<div class="rail-section">${g.label}</div>${items}`;
  }).join("");

  const e = state.env;
  const statuses: [string, string | null, string][] = [
    ["QEMU", e ? (e.qemu ? "ok" : "err") : null, e?.qemu ? "就绪" : "缺失"],
    ["镜像", e ? (e.images_ready ? "ok" : "warn") : null,
      e?.images_ready ? "就绪" : "待构建"],
  ];

  rail.innerHTML = `
    ${groups}
    <div class="rail-spacer"></div>
    <div style="border-top:1px solid var(--line-soft);padding-top:10px;margin-top:6px">
      ${statuses
        .map(
          ([name, cls, val]) => `
        <div class="env-row">
          <span class="rail-dot ${cls ?? ""}"></span>
          <span class="env-name">${name}</span>
          <span class="env-val">${val}</span>
        </div>`
        )
        .join("")}
    </div>
  `;

  rail.querySelectorAll<HTMLButtonElement>(".rail-item").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.page = btn.dataset.page as PageId;
      paintRail();
      paintView();
    });
  });
}

function paintHead(nav: NavDef) {
  document.querySelector("#page-name")!.textContent = nav.title;
  const acts = document.querySelector<HTMLElement>("#head-actions")!;

  if (state.page === "vms") {
    acts.innerHTML = `
      <button class="btn btn-sm" id="h-new">${icon("plus", 13)} 新建</button>
    `;
    acts.querySelector("#h-new")?.addEventListener("click", createVm);
  } else {
    acts.innerHTML = "";
  }
}

function paintView() {
  const nav = NAV.find((n) => n.id === state.page)!;
  paintHead(nav);

  const view = document.querySelector<HTMLElement>("#view")!;
  view.innerHTML = "";

  const map: Record<PageId, (r: HTMLElement) => void> = {
    vms: renderVms,
    root: renderRoot,
    lsp: renderLsp,
    "magisk-mod": renderMagiskModules,
    images: renderImages,
    apps: renderApps,
    diagnose: renderDiagnose,
    compat: renderCompat,
    settings: renderSettings,
  };
  map[state.page](view);
}

// LSP 与模块页的共享状态适配器
function lspState() {
  const vm = state.vms.find((v) => v.id === state.currentVmId);
  return {
    lsp: state.lsp,
    lspLoading: state.lspLoading,
    modules: state.modules,
    moduleResults: state.moduleResults,
    moduleQueries: state.moduleQueries,
    moduleBusy: state.moduleBusy,
    modulesLoading: state.modulesLoading,
    moduleQuery: state.moduleQuery,
    moduleOrg: state.moduleOrg,
    modulesPage: 1,
    vms: state.vms,
    currentVmId: state.currentVmId,
    log: (m: string, lv: "info" | "ok" | "warn" | "err" = "info") => log(m, lv),
    _vm: vm,
  };
}

function syncFromLspState(s: ReturnType<typeof lspState>) {
  state.vms = s.vms;
  state.moduleResults = s.moduleResults;
  state.moduleQueries = s.moduleQueries;
  state.moduleBusy = s.moduleBusy;
  state.lsp = s.lsp;
  state.lspLoading = s.lspLoading;
  state.modules = s.modules;
  state.modulesLoading = s.modulesLoading;
  state.moduleQuery = s.moduleQuery;
  state.moduleOrg = s.moduleOrg;
}

function renderLsp(view: HTMLElement) {
  const s = lspState();
  renderLspPage(view, s as any, () => {
    syncFromLspState(s);
    paintView();
  });
}

function renderMagiskModules(view: HTMLElement) {
  const s = lspState();
  renderMagiskModulesPage(view, s as any, () => {
    syncFromLspState(s);
    paintView();
  });
}

// ------------------------------------------------------------------ 系统镜像页

function renderImages(view: HTMLElement) {
  const st = {
    images: state.aospImages,
    loading: state.aospLoading,
    progress: state.aospProgress,
    log: (m: string, lv: "info" | "ok" | "warn" | "err" = "info") => log(m, lv),
    onDone: () => {
      // 镜像变化后刷新环境状态
      api.envStatus().then((e) => {
        state.env = e;
        paintRail();
      });
    },
  };

  renderImagesPage(view, st, () => {
    state.aospImages = st.images;
    state.aospLoading = st.loading;
    state.aospProgress = st.progress;
    paintView();
  });
}

// ------------------------------------------------------------------ 虚拟机页

function envTile(label: string, ok: boolean, value: string): string {
  return `
    <div class="env-tile">
      <span class="rail-dot ${ok ? "ok" : "warn"}"></span>
      <div class="col" style="gap:1px;min-width:0">
        <span class="et-k">${label}</span>
        <span class="et-v">${esc(value)}</span>
      </div>
    </div>`;
}

/** 生成版本下拉的 option 列表 */
function buildVersionOptions(managerId: string, currentVersion: string): string {
  if (!managerId || managerId === "none") {
    return `<option value="">—</option>`;
  }

  const m = state.managers.find((x) => x.id === managerId);
  const downloaded = [...(m?.versions.filter((v) => v.downloaded) ?? [])];
  for (const boot of state.preparedBoots[state.currentVmId ?? ""] ?? []) {
    const prefix = `${managerId}@`;
    if (boot.root.startsWith(prefix)) {
      const version = boot.root.slice(prefix.length);
      if (!downloaded.some((item) => item.version === version)) downloaded.push({ version } as RootVersion);
    }
  }
  if (currentVersion && !downloaded.some((v) => v.version === currentVersion)) {
    return `<option value="${esc(currentVersion)}" selected>v${esc(currentVersion)} · 已准备/缓存待核验</option>` + downloaded.map((v) => `<option value="${v.version}">v${esc(v.version)}</option>`).join("");
  }

  if (downloaded.length === 0) {
    return `<option value="">无可用版本，请先下载或填写准备版本</option>`;
  }

  return downloaded
    .map(
      (v) =>
        `<option value="${v.version}" ${currentVersion === v.version ? "selected" : ""}>v${esc(v.version)}</option>`
    )
    .join("");
}

/** 生成 LSPosed 版本下拉 */
function buildLspOptions(current: string): string {
  const all = state.lsp?.versions.filter((v) => v.downloaded) ?? [];
  const items = [
    `<option value="none" ${current === "none" ? "selected" : ""}>不安装</option>`,
  ];

  const seen = new Set<string>();
  for (const v of all) {
    const key = `${v.version}-${v.variant}`;
    if (key !== "1.9.2-zygisk") continue;
    if (seen.has(key)) continue;
    seen.add(key);
    const label =
      v.variant === "zygisk" ? "Zygisk" : v.variant === "riru" ? "Riru" : v.variant;
    items.push(
      `<option value="${key}" ${current === key ? "selected" : ""}>v${esc(v.version)} · ${label}</option>`
    );
  }

  if (current !== "none" && !seen.has(current)) items.push(`<option value="${esc(current)}" selected>${esc(current)} · 仅保留配置，自动应用未支持</option>`);
  return items.join("");
}

function renderVms(view: HTMLElement) {
  const attr = (s: string) => esc(s).replace(/"/g, "&quot;");
  const vm = state.vms.find((v) => v.id === state.currentVmId);
  const running = vm ? state.running.has(vm.id) : false;
  if (vm) {
    const key = JSON.stringify([vm.backend, vm.emulator_image_dir, vm.prepared_root, vm.prepared_ramdisk, running]);
    if (state.preparedKeys[vm.id] !== key) {
      state.preparedKeys[vm.id] = key;
      api.listPreparedBoots(vm.id).then((entries) => {
        if (state.preparedKeys[vm.id] !== key) return;
        state.preparedBoots[vm.id] = entries;
        if (state.page === "vms" && state.currentVmId === vm.id) paintView();
      }).catch((e) => { state.preparedBoots[vm.id] = []; log(`准备文件索引未就绪: ${e}`, "warn"); });
    }
  }

  // Root 选择：root_manager 存 "manager@version" 或 "none"
  const rootVal = vm?.root_manager ?? "none";
  const [currentManager, currentVersion] = rootVal.includes("@")
    ? rootVal.split("@")
    : [rootVal, ""];

  // 只列出已下载版本的管理器
  const availableManagers = state.managers.filter((m) => m.id === "magisk" && m.versions.some((v) => v.downloaded));

  view.innerHTML = `
    <div class="page-head">
      <div>
        <h1>虚拟机</h1>
        <p>管理和运行 Android 虚拟机</p>
      </div>
    </div>

    <div class="split">
      <!-- 左侧：虚拟机列表 -->
      <div class="panel">
        <div class="panel-head">
          <div class="titles"><div class="t">列表</div></div>
          <button class="btn btn-ghost btn-icon" id="btn-add">${icon("plus", 14)}</button>
        </div>
        <div class="vm-list">
          ${
            state.vms.length === 0
              ? `<div class="empty" style="padding:32px 12px">
                   <div class="t">暂无虚拟机</div>
                   <div class="d">点击右上角 + 创建</div>
                 </div>`
              : state.vms
                  .map(
                    (v) => `
                <div class="vm-item ${v.id === state.currentVmId ? "active" : ""}" data-vm="${v.id}">
                  <span class="rail-dot ${state.running.has(v.id) ? "ok" : ""}"></span>
                  <span class="name">${esc(v.name)}</span>
                </div>`
                  )
                  .join("")
          }
        </div>
        ${
          state.currentVmId
            ? `<div style="padding:0 8px 10px">
                 <button class="btn btn-ghost btn-sm" id="btn-del"
                         style="width:100%;color:var(--fg-faint);justify-content:flex-start">
                   ${icon("trash", 12)} 删除
                 </button>
               </div>`
            : ""
        }
      </div>

      <!-- 右侧：详情 -->
      <div class="col" style="gap:16px;min-height:0">
        <!-- 指标 -->
        <div class="stats">
          <div class="stat"><div class="k">内存</div><div class="v">${vm?.memory_mb ?? "—"}<small>MB</small></div></div>
          <div class="stat"><div class="k">CPU</div><div class="v">${vm?.cores ?? "—"}<small>核</small></div></div>
          <div class="stat"><div class="k">ADB</div><div class="v">${vm?.adb_port ?? "—"}</div></div>
          <div class="stat"><div class="k">Root</div><div class="v" style="font-size:14px">
            ${rootVal === "none" ? "无" : `${esc(currentManager)} ${esc(currentVersion ? "v" + currentVersion : "")}`}
          </div></div>
        </div>

        <!-- 配置 -->
        <div class="panel">
          <div class="panel-head">
            <div class="titles">
              <div class="t">配置</div>
              <div class="s">${vm ? "修改后自动保存" : "选择左侧虚拟机后可编辑"}</div>
            </div>
          </div>
          <div class="panel-body">
            <div class="form-grid">
              <div class="field">
                <label>内存 (MB)</label>
                <input class="input" type="number" id="f-mem" value="${vm?.memory_mb ?? 6144}"
                       min="1024" max="16384" step="512" ${vm && !running ? "" : "disabled"}>
              </div>
              <div class="field">
                <label>CPU 核心</label>
                <input class="input" type="number" id="f-cpu" value="${vm?.cores ?? 8}"
                       min="1" max="32" ${vm && !running ? "" : "disabled"}>
              </div>
              <div class="field">
                <label>Root 管理器</label>
                <select class="select" id="f-root" ${vm ? "" : "disabled"}>
                  <option value="none" ${currentManager === "none" ? "selected" : ""}>无 Root</option>
                  ${currentManager !== "none" && currentManager !== "magisk" ? `<option value="${esc(currentManager)}" selected>${esc(currentManager)} · 当前镜像未支持</option>` : ""}
                  ${!availableManagers.some((m) => m.id === "magisk") ? `<option value="magisk" ${currentManager === "magisk" ? "selected" : ""}>Magisk · 已准备 ramdisk</option>` : ""}
                  ${availableManagers
                    .map(
                      (m) =>
                        `<option value="${m.id}" ${currentManager === m.id ? "selected" : ""}>${esc(m.name)}</option>`
                    )
                    .join("")}
                </select>
              </div>
              <div class="field">
                <label>版本</label>
                <select class="select" id="f-root-ver"
                        ${vm && currentManager !== "none" ? "" : "disabled"}>
                  ${buildVersionOptions(currentManager, currentVersion)}
                </select>
              </div>
            </div>
            <div class="form-grid" style="margin-top:12px">
              <div class="field">
                <label>LSPosed 版本</label>
                <select class="select" id="f-lsp-ver" ${vm ? "" : "disabled"}>
                  ${buildLspOptions(vm?.lsp_version ?? "none")}
                </select>
                <span class="text-xs faint">自动应用仅公开版 1.9.2 Zygisk；需已启用 Zygisk，安装后重启验证。</span>
              </div>
            </div>
              <div class="field">
                <label>ADB 端口</label>
                <input class="input" type="number" id="f-port" value="${vm?.adb_port ?? 5555}"
                       min="1024" max="65535" ${vm && !running && vm.backend !== "android-emulator" ? "" : "disabled"}>
              </div>
          </div>
        </div>

        <div class="panel"><div class="panel-head"><div class="titles"><div class="t">运行后端与准备式 Root</div><div class="s">仅检测加速，不操作 AEHD/MuMu 驱动；KSU/APatch 当前镜像未支持</div></div></div>
          <div class="panel-body"><div class="form-grid">
            <div class="field"><label>后端</label><select id="f-backend" class="select" ${!vm || running ? "disabled" : ""}>
              <option value="android-emulator" ${vm?.backend === "android-emulator" ? "selected" : ""}>Android Emulator · 硬件加速</option>
              <option value="legacy-qemu" ${vm?.backend !== "android-emulator" ? "selected" : ""}>旧 QEMU · ARM64/TCG</option></select></div>
            <div class="field"><label>加速检查要求</label><select id="f-accel" class="select" ${!vm || running ? "disabled" : ""}>
              <option value="aehd" ${vm?.accel === "aehd" ? "selected" : ""}>须确认 AEHD</option><option value="auto" ${vm?.accel !== "aehd" ? "selected" : ""}>已安装可用加速器（不切驱动）</option></select></div>
            ${[["f-sdk", "Android SDK 目录", vm?.sdk_path ?? ""], ["f-avd-home", "源 AVD home（含 name.avd）", vm?.avd_home ?? ""], ["f-avd", "源 AVD 名称", vm?.avd_name ?? ""], ["f-image", "完整 x86_64 系统镜像目录", vm?.emulator_image_dir ?? ""], ["f-console", "Emulator console 偶数端口", String(vm?.console_port ?? 5554)], ["f-prepared-root", "已准备 ramdisk 的版本标识（magisk@28.0）", vm?.prepared_root ?? ""], ["f-ramdisk", "已准备 ramdisk 文件路径", vm?.prepared_ramdisk ?? ""], ["f-magisk-path", "设备内 Magisk 可执行路径", vm?.magisk_path ?? "/debug_ramdisk/magisk"]].map(([id, label, value]) => `<div class="field"><label>${label}</label><input class="input" id="${id}" value="${attr(value)}" ${!vm || running ? "disabled" : ""}></div>`).join("")}
          </div>
          <div class="text-sm faint" style="margin-top:12px">期望 Root：${esc(rootVal)}；上次验证：${esc(vm?.root_applied ?? "未验证")}<br>期望 LSP：${esc(vm?.lsp_version ?? "none")}；检测到安装版本：${esc(vm?.lsp_applied ?? "未检测")}（非完整激活证明）；状态：${esc(vm?.lsp_state || "未核验")}；${vm?.pending_restart ? "待应用/重启验证" : "无待重启操作（激活状态见核验结果）"}</div>
          <div class="row" style="margin-top:10px"><button class="btn btn-sm" id="act-magisk-init" ${!running ? "disabled" : ""}>初始化 Magisk 运行环境</button><button class="btn btn-sm" id="act-lsp-apply" ${!running ? "disabled" : ""}>应用所选 LSPosed / 禁用</button><button class="btn btn-sm" id="act-verify" ${!running ? "disabled" : ""}>验证实际版本</button></div>
          <div class="text-xs faint">选择只保存期望状态。Magisk 使用匹配的准备文件，复制至每 VM boot；版本未准备则启动报错。源镜像与源AVD保持只读。首次请在 Magisk 管理器明确授权 Shell；初始化不执行 adb root 或修改授权策略。准备 ramdisk 须保留官方 cpio backup（.backup/init.xz）、KEEPVERITY/KEEPFORCEENCRYPT 与正确 PREINITDEVICE；LSPosed 完整激活请在管理器复核。</div>
          </div></div>
        <!-- 操作 -->
        <div class="row">
          <button class="btn btn-accent btn-lg" id="act-start" ${!vm || running || state.starting.has(vm.id) ? "disabled" : ""}>
            ${icon("play", 13)} ${vm && state.starting.has(vm.id) ? "启动中" : "启动"}
          </button>
          <label class="text-xs faint" style="display:flex;align-items:center;gap:6px">
            <input type="checkbox" id="f-headless" ${state.headless ? "checked" : ""} ${running || (vm && state.starting.has(vm.id)) ? "disabled" : ""}>
            后台运行（不打开模拟器窗口）
          </label>
          <button class="btn btn-danger" id="act-stop" ${!running ? "disabled" : ""}>
            ${icon("stop", 12)} 停止
          </button>
          <div class="spacer"></div>
          ${
            running
              ? `<span class="chip ok"><span class="dot"></span>运行中</span>`
              : `<span class="chip plain"><span class="dot"></span>已停止</span>`
          }
        </div>

        <!-- 运行环境 -->
        <div class="env-grid">
          ${vm?.backend === "android-emulator" ? envTile("Emulator", false, vm.sdk_path ? "SDK已配置 · 启动时检查" : "请配置SDK") : envTile("QEMU", state.env?.qemu ?? false, state.env?.qemu ? "就绪" : "缺失")}
          ${vm?.backend === "android-emulator" ? envTile("ADB", false, "使用该SDK/platform-tools · 精确serial") : envTile("ADB", state.env?.adb ?? false, state.env?.adb ? "就绪" : "缺失")}
          ${envTile("scrcpy", state.env?.scrcpy ?? false, state.env?.scrcpy ? "就绪" : "未装")}
          ${vm?.backend === "android-emulator" ? envTile("x86_64镜像", false, vm.emulator_image_dir ? "已配置 · 启动时检查" : "请配置完整系统镜像目录") : envTile("基础镜像", state.env?.images_ready ?? false,
            state.env?.images_ready ? "就绪" : `缺 ${state.env?.missing_images.length ?? 0} 项`)}
          ${envTile("Overlay", (state.overlays.length > 0),
            `${state.overlays.filter((o) => o.has_binary).length} 已就绪`)}
          ${envTile("后端/加速", false, vm?.backend === "android-emulator" ? `${vm.accel} · 启动时检测` : "TCG · 旧后端")}
        </div>

        <!-- 日志 -->
        <div class="panel" style="flex:1;display:flex;flex-direction:column;min-height:0">
          <div class="panel-head">
            <div class="titles"><div class="t">运行日志</div></div>
            <button class="btn btn-ghost btn-sm" id="btn-clear">清空</button>
          </div>
          <div class="panel-body tight" style="flex:1;min-height:0">
            <div class="console" id="console" style="height:100%;max-height:none"></div>
          </div>
        </div>
      </div>
    </div>
  `;

  view.querySelectorAll<HTMLElement>(".vm-item").forEach((el) =>
    el.addEventListener("click", () => {
      state.currentVmId = el.dataset.vm!;
      paintView();
    })
  );

  view.querySelector("#btn-add")?.addEventListener("click", createVm);
  view.querySelector("#btn-del")?.addEventListener("click", deleteVm);
  if (vm && running) {
    const consolePanel = view.querySelector("#console")?.closest(".panel");
    consolePanel?.insertAdjacentElement("beforebegin", createGuestPreview(vm.id));
  }
  view.querySelector("#act-start")?.addEventListener("click", startVm);
  view.querySelector<HTMLInputElement>("#f-headless")?.addEventListener("change", (event) => {
    state.headless = (event.currentTarget as HTMLInputElement).checked;
  });
  view.querySelector("#act-stop")?.addEventListener("click", stopVm);
  view.querySelector("#btn-clear")?.addEventListener("click", () => {
    state.logs = [];
    paintConsole();
  });

  // 配置自动保存
  const saveConfig = async () => {
    const original = state.vms.find((x) => x.id === state.currentVmId);
    if (!original) return;
    const v = { ...original };

    v.memory_mb = +(view.querySelector<HTMLInputElement>("#f-mem")!.value);
    v.cores = +(view.querySelector<HTMLInputElement>("#f-cpu")!.value);
    v.adb_port = +(view.querySelector<HTMLInputElement>("#f-port")!.value);
    v.lsp_version = view.querySelector<HTMLSelectElement>("#f-lsp-ver")!.value;
    v.backend = view.querySelector<HTMLSelectElement>("#f-backend")!.value as VmConfig["backend"];
    v.accel = view.querySelector<HTMLSelectElement>("#f-accel")!.value;
    v.sdk_path = view.querySelector<HTMLInputElement>("#f-sdk")!.value.trim();
    v.avd_home = view.querySelector<HTMLInputElement>("#f-avd-home")!.value.trim();
    v.avd_name = view.querySelector<HTMLInputElement>("#f-avd")!.value.trim();
    v.emulator_image_dir = view.querySelector<HTMLInputElement>("#f-image")!.value.trim();
    v.console_port = +(view.querySelector<HTMLInputElement>("#f-console")!.value);
    if (v.backend === "android-emulator") v.adb_port = v.console_port + 1;
    v.prepared_root = view.querySelector<HTMLInputElement>("#f-prepared-root")!.value.trim();
    v.prepared_ramdisk = view.querySelector<HTMLInputElement>("#f-ramdisk")!.value.trim();
    v.magisk_path = view.querySelector<HTMLInputElement>("#f-magisk-path")!.value.trim();

    const mgr = view.querySelector<HTMLSelectElement>("#f-root")!.value;
    const ver = view.querySelector<HTMLSelectElement>("#f-root-ver");
    if (mgr === "none") {
      v.root_manager = "none";
    } else if (ver?.value) {
      v.root_manager = `${mgr}@${ver.value}`;
    } else if (v.prepared_root.startsWith(`${mgr}@`) && v.prepared_root.length > mgr.length + 1) {
      v.root_manager = v.prepared_root;
    } else {
      log(`请先为 ${mgr} 选择或填写匹配版本；原配置保持不变`, "warn");
      return;
    }

    try {
      await api.saveVm(v);
      state.vms = await api.listVms();
      log(`期望配置已保存；实际状态请启动后验证`, "ok");
      paintView();
    } catch (e) {
      log(`保存失败: ${e}`, "err");
      state.vms = await api.listVms(); paintView();
    }
  };

  ["f-mem", "f-cpu", "f-port", "f-backend", "f-accel", "f-sdk", "f-avd-home", "f-avd", "f-image", "f-console", "f-prepared-root", "f-ramdisk", "f-magisk-path"].forEach((id) =>
    view.querySelector(`#${id}`)?.addEventListener("change", saveConfig)
  );

  // Root 管理器切换 → 联动版本下拉
  view.querySelector("#f-root")?.addEventListener("change", () => {
    const mgr = view.querySelector<HTMLSelectElement>("#f-root")!.value;
    const verSel = view.querySelector<HTMLSelectElement>("#f-root-ver")!;

    if (mgr === "none") {
      verSel.innerHTML = `<option value="">—</option>`;
      verSel.disabled = true;
    } else {
      verSel.innerHTML = buildVersionOptions(mgr, "");
      verSel.disabled = !verSel.querySelector('option[value]:not([value=""])');
    }
    saveConfig();
  });

  view.querySelector("#f-root-ver")?.addEventListener("change", saveConfig);
  view.querySelector("#f-lsp-ver")?.addEventListener("change", saveConfig);
  view.querySelector("#act-magisk-init")?.addEventListener("click", async () => {
    const id = state.currentVmId; if (!id) return;
    try { const r = await api.initializeVmMagisk(id); if (!r.ok) throw new Error(r.stderr || r.stdout);
      log("该VM的Magisk运行资源已部署；LSPosed仍需preinit规则和管理器完整激活复核", "ok");
    } catch(e) { log(`Magisk初始化失败: ${e}`, "err"); }
  });
  view.querySelector("#act-lsp-apply")?.addEventListener("click", async () => {
    const id = state.currentVmId; if (!id) return;
    try { const r = await api.applyVmLsposed(id); if (!r.ok) throw new Error(r.stderr || r.stdout);
      log(`LSPosed 应用已完成；请仅重启该 VM 后验证。${r.stdout}`, "ok");
      state.vms = await api.listVms(); paintView();
    } catch (e) { log(`LSPosed 应用失败: ${e}`, "err"); }
  });
  view.querySelector("#act-verify")?.addEventListener("click", async () => {
    const id = state.currentVmId; if (!id) return;
    try { const r = await api.verifyVmRuntime(id); log(`Root=${r.root_verified} / LSP=${r.lsp_verified}（${r.lsp_state}）；${r.message}`, !r.root_verified ? "err" : r.pending_restart || !r.lsp_verified ? "warn" : "ok");
      state.vms = await api.listVms(); paintView();
    } catch (e) { log(`版本验证失败: ${e}`, "err"); }
  });

  paintConsole();
}

function paintConsole() {
  const el = document.querySelector<HTMLElement>("#console");
  if (!el) return;
  el.innerHTML = state.logs
    .map(
      (l) => `<div class="line ${l.level}">
        <span class="time">${l.time}</span>
        <span class="msg">${esc(l.text)}</span>
      </div>`
    )
    .join("");
  el.scrollTop = el.scrollHeight;
}

// ------------------------------------------------------------------ Root 页

function renderRoot(view: HTMLElement) {
  if (state.managersLoading) {
    view.innerHTML = `
      <div class="page-head">
        <div><h1>Root 管理器</h1><p>正在从 GitHub 查询版本列表...</p></div>
      </div>
      <div class="panel" style="flex:1">
        <div class="empty">
          ${icon("refresh", 34)}
          <div class="t">正在加载</div>
          <div class="d">从 GitHub Releases 获取各管理器的版本清单</div>
        </div>
      </div>
    `;
    return;
  }

  const readyCount = state.managers.filter((m) =>
    m.versions.some((v) => v.downloaded)
  ).length;

  const offline = state.managers.filter((m) => m.status !== "ok" && m.kind !== "manual");

  view.innerHTML = `
    <div class="page-head">
      <div>
        <h1>Root 管理器</h1>
        <p>可插拔的 Root 授权层 · 按需下载版本，下载后可在虚拟机页切换</p>
      </div>
      <div class="spacer"></div>
      <button class="btn btn-sm" id="btn-refresh-root">${icon("refresh", 13)} 刷新版本</button>
    </div>

    ${
      offline.length
        ? `<div class="panel" style="border-color:rgba(245,165,36,.3)">
             <div class="panel-body" style="padding:14px 18px;display:flex;align-items:center;gap:10px">
               ${icon("alert", 16)}
               <span class="text-sm" style="color:var(--warn)">
                 ${offline.length} 个管理器的版本列表获取失败（GitHub API 不可达）。
                 可点击右上角刷新重试。
               </span>
             </div>
           </div>`
        : ""
    }

    <div class="stats" style="grid-template-columns:repeat(3,1fr)">
      <div class="stat">
        <div class="k">管理器</div>
        <div class="v">${state.managers.filter((m) => m.kind !== "manual").length}</div>
      </div>
      <div class="stat">
        <div class="k">可选版本</div>
        <div class="v">${state.managers.reduce((n, m) => n + m.versions.length, 0)}</div>
      </div>
      <div class="stat">
        <div class="k">已下载</div>
        <div class="v">${state.managers.reduce((n, m) => n + m.versions.filter((v) => v.downloaded).length, 0)}</div>
      </div>
    </div>

    <div style="display:flex;flex-direction:column;gap:14px">
      ${state.managers.map((m) => managerCard(m)).join("")}
    </div>
  `;

  view.querySelector("#btn-refresh-root")?.addEventListener("click", async () => {
    const btn = view.querySelector<HTMLButtonElement>("#btn-refresh-root")!;
    btn.disabled = true;
    btn.innerHTML = "刷新中";
    await loadManagers();
    try { state.lsp = await api.listLsposed(); } catch {}
    try { state.aospImages = await api.listAospImages(); } catch {}
    paintView();
    log("版本列表已刷新", "ok");
  });

  // 版本展开/收起
  view.querySelectorAll<HTMLElement>("[data-toggle-versions]").forEach((el) => {
    el.addEventListener("click", (ev) => {
      // 点在按钮上时不触发折叠
      if ((ev.target as HTMLElement).closest("button[data-dl],button[data-rm]")) return;
      const id = el.dataset.toggleVersions!;
      const body = view.querySelector<HTMLElement>(`#versions-${id}`);
      const arrow = view.querySelector<HTMLElement>(`#arrow-${id}`);
      if (!body) return;
      const hidden = body.style.display === "none";
      body.style.display = hidden ? "flex" : "none";
      if (arrow) arrow.style.transform = hidden ? "rotate(90deg)" : "rotate(0deg)";
    });
  });

  // 下载
  view.querySelectorAll<HTMLButtonElement>("[data-dl]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const [manager, version, source] = btn.dataset.dl!.split("|");
      await downloadRoot(manager, version, source as "direct" | "mirror", btn);
    });
  });

  // 删除
  view.querySelectorAll<HTMLButtonElement>("[data-rm]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const [manager, version] = btn.dataset.rm!.split("|");
      if (!confirm(`删除 ${manager} ${version}？`)) return;
      try {
        await api.removeRootVersion(manager, version);
        await loadManagers();
    try { state.lsp = await api.listLsposed(); } catch {}
    try { state.aospImages = await api.listAospImages(); } catch {}
        paintView();
        log(`已删除 ${manager} ${version}`, "ok");
      } catch (e) {
        log(`删除失败: ${e}`, "err");
      }
    });
  });

  // 展开其余版本
  view.querySelectorAll<HTMLButtonElement>("[data-more]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const id = btn.dataset.more!;
      const box = view.querySelector<HTMLElement>(`#more-${id}`);
      if (!box) return;
      const hidden = box.style.display === "none";
      box.style.display = hidden ? "flex" : "none";
      btn.textContent = hidden ? "收起" : btn.textContent;
      if (hidden) btn.style.display = "none";
    });
  });

  // 版本搜索
  view.querySelectorAll<HTMLButtonElement>("[data-vsearch]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const mid = btn.dataset.vsearch!;
      const input = view.querySelector<HTMLInputElement>(`#vsearch-${mid}`);
      const box = view.querySelector<HTMLElement>(`#vresult-${mid}`);
      if (!input || !box) return;

      const q = input.value.trim();
      try {
        const found = await api.searchRootVersions(mid, q);
        if (found.length === 0) {
          box.style.display = "flex";
          box.innerHTML = `<div class="text-xs faint" style="padding:6px 0">未找到匹配版本</div>`;
          return;
        }

        const mgr = state.managers.find((x) => x.id === mid);
        box.style.display = "flex";
        box.innerHTML = `
          <div class="text-xs faint" style="padding:4px 0">
            找到 ${found.length} 个版本（来自仓库全部 release）
          </div>
          ${found
            .map((ver) => {
              const full = mgr?.versions.find((v) => v.version === ver);
              if (full) return versionRow(mgr!, full);
              // 缓存里有但管理器列表没带的版本
              return `
                <div class="row" style="padding:8px 12px;background:var(--bg-input);
                            border:1px solid var(--line-soft);border-radius:var(--r-sm);gap:10px">
                  <span class="mono text-sm" style="font-weight:600;min-width:72px">v${esc(ver)}</span>
                  <span class="text-xs faint">未在当前清单中</span>
                  <div class="spacer"></div>
                </div>`;
            })
            .join("")}
        `;

        // 绑定新出现的下载按钮
        box.querySelectorAll<HTMLButtonElement>("[data-dl]").forEach((b) => {
          b.addEventListener("click", async () => {
            const [manager, version, source] = b.dataset.dl!.split("|");
            await downloadRoot(manager, version, source as "direct" | "mirror", b);
          });
        });
      } catch (e) {
        log(`版本搜索失败: ${e}`, "err");
      }
    });
  });

  // 回车触发搜索
  view.querySelectorAll<HTMLInputElement>("[id^='vsearch-']").forEach((inp) => {
    inp.addEventListener("keydown", (e) => {
      if ((e as KeyboardEvent).key === "Enter") {
        const mid = inp.id.replace("vsearch-", "");
        view.querySelector<HTMLButtonElement>(`[data-vsearch="${mid}"]`)?.click();
      }
    });
  });
}

function managerCard(m: RootManager): string {
  const downloaded = m.versions.filter((v) => v.downloaded);
  const kindLabel =
    m.kind === "ready"
      ? `<span class="chip ok"><span class="dot"></span>可直接使用</span>`
      : m.kind === "needs-kernel"
        ? `<span class="chip warn"><span class="dot"></span>需内核适配</span>`
        : `<span class="chip plain"><span class="dot"></span>手动添加</span>`;

  const openByDefault = downloaded.length > 0;

  // 已下载版本优先，其次精选版本，其余归入更多
  const shown = m.versions.filter((v) => v.downloaded || v.featured);
  const rest = m.versions.filter((v) => !v.downloaded && !v.featured);

  return `
    <div class="panel">
      <div class="panel-head" style="cursor:pointer" data-toggle-versions="${m.id}">
        <span id="arrow-${m.id}" style="display:inline-flex;transition:transform .2s;
              transform:rotate(${openByDefault ? 90 : 0}deg);color:var(--fg-faint)">
          ${icon("chevron", 14)}
        </span>
        <div class="titles">
          <div class="t row" style="gap:8px">
            <span>${esc(m.name)}</span>
            ${kindLabel}
            ${m.zygisk ? `<span class="chip info">Zygisk</span>` : ""}
            ${downloaded.length ? `<span class="chip ok">${downloaded.length} 个已下载</span>` : ""}
          </div>
          <div class="s">${esc(m.desc)}</div>
        </div>
        <span class="text-xs faint mono">${esc(m.license)}</span>
      </div>

      <div id="versions-${m.id}" style="display:${openByDefault ? "flex" : "none"}; max-height:460px; overflow-y:auto;
           flex-direction:column;padding:12px 20px 16px;gap:8px">

        <!-- 版本搜索 -->
        <div class="row" style="gap:8px;margin-bottom:4px">
          <input class="input" id="vsearch-${m.id}" placeholder="搜索版本号（可搜全部历史版本）"
                 style="flex:1;height:30px;font-size:12px">
          <button class="btn btn-sm" data-vsearch="${m.id}">${icon("search", 12)} 搜索</button>
        </div>
        <div id="vresult-${m.id}" style="display:none;flex-direction:column;gap:6px"></div>

        ${
          m.versions.length === 0
            ? `<div class="text-sm faint" style="padding:8px 0">此管理器无预置版本。</div>`
            : `
              ${shown.length ? `<div class="text-xs faint" style="padding:2px 0">常用与已下载</div>` : ""}
              ${shown.map((v) => versionRow(m, v)).join("")}
              ${
                rest.length
                  ? `<button class="btn btn-ghost btn-sm" data-more="${m.id}"
                             style="align-self:flex-start;margin-top:4px">
                       展开其余 ${rest.length} 个版本
                     </button>
                     <div id="more-${m.id}" style="display:none;flex-direction:column;gap:8px">
                       ${rest.map((v) => versionRow(m, v)).join("")}
                     </div>`
                  : ""
              }
            `
        }
      </div>
    </div>
  `;
}

function versionRow(m: RootManager, v: RootVersion): string {
  const key = `${m.id}-${v.version}`;
  const noUrl = !v.url;

  return `
    <div class="row" style="padding:9px 12px;background:var(--bg-input);
                border:1px solid var(--line-soft);border-radius:var(--r-sm);gap:10px">
      <span class="mono text-sm" style="font-weight:600;min-width:72px">v${esc(v.version)}</span>
      ${v.prerelease ? `<span class="chip warn" style="font-size:9px">预发布</span>` : ""}
      <span class="text-xs faint" style="min-width:64px">${esc(v.size)}</span>
      <span class="text-xs faint" style="min-width:82px">${esc(v.date)}</span>
      <div style="flex:1"></div>
      ${
        v.downloaded
          ? `<span class="chip ok"><span class="dot"></span>已下载</span>
             <button class="btn btn-ghost btn-sm" data-rm="${m.id}|${v.version}"
                     style="color:var(--fg-faint)">删除</button>`
          : noUrl
            ? `<span class="text-xs faint">无下载地址</span>`
            : `<div style="width:150px;height:4px;background:var(--bg-raised);
                           border-radius:2px;overflow:hidden" id="prog-${key}">
                 <div style="height:100%;width:0;background:var(--green);transition:width .2s"
                      id="progbar-${key}"></div>
               </div>
               <span class="text-xs faint" id="progtext-${key}" style="min-width:50px"></span>
               <button class="btn btn-sm" data-dl="${m.id}|${v.version}|direct"
                       title="从 GitHub 直连下载">
                 ${icon("download", 12)} 直连
               </button>
               <button class="btn btn-sm" data-dl="${m.id}|${v.version}|mirror"
                       title="通过国内镜像下载">
                 ${icon("download", 12)} 镜像
               </button>`
      }
    </div>
  `;
}

async function downloadRoot(
  manager: string,
  version: string,
  source: "direct" | "mirror",
  btn: HTMLButtonElement
) {
  // 找到该版本的下载地址
  const m = state.managers.find((x) => x.id === manager);
  const v = m?.versions.find((x) => x.version === version);
  if (!v || !v.url) {
    log("该版本没有下载地址", "err");
    return;
  }

  const key = `${manager}-${version}`;
  btn.disabled = true;
  btn.innerHTML = "连接中";

  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<DownloadProgress>("root-download-progress", (ev) => {
    const p = ev.payload;
    if (p.manager !== manager || p.version !== version) return;

    const bar = document.querySelector<HTMLElement>(`#progbar-${key}`);
    const text = document.querySelector<HTMLElement>(`#progtext-${key}`);
    if (bar) bar.style.width = `${Math.min(100, p.percent)}%`;
    if (text) text.textContent = p.message;

    if (btn) {
      if (p.status === "downloading") btn.innerHTML = `${p.percent.toFixed(0)}%`;
      else if (p.status === "extracting") btn.innerHTML = "解包";
    }
  });

  const srcLabel = source === "mirror" ? "镜像" : "直连";

  try {
    await api.downloadRoot(manager, version, v.url, v.asset_name, source);
    log(`${manager} ${version}（${srcLabel}）安装完成`, "ok");
    await loadManagers();
    try { state.lsp = await api.listLsposed(); } catch {}
    try { state.aospImages = await api.listAospImages(); } catch {}
    paintView();
  } catch (e) {
    log(`${manager} ${version} ${srcLabel}下载失败: ${String(e)}`, "err");
    btn.disabled = false;
    btn.innerHTML = `${icon("download", 12)} 重试`;
  } finally {
    unlisten();
  }
}

async function loadManagers() {
  try {
    state.managers = await api.listRootManagers();
  } catch (e) {
    log(`加载管理器失败: ${e}`, "err");
  }
}

// ------------------------------------------------------------------ 其他页

function devicePageTarget(view: HTMLElement, title: string, subtitle: string): VmConfig | undefined {
  const vm = state.vms.find((item) => item.id === state.currentVmId);
  const attr = (value: string) => esc(value).replace(/"/g, "&quot;");
  view.innerHTML = `<div class="page-head"><div><h1>${title}</h1><p>${subtitle}</p></div></div>
    <div class="panel"><div class="panel-body"><label>目标虚拟机
      <select class="input" id="device-page-vm"><option value="">请选择</option>
      ${state.vms.map((item) => `<option value="${attr(item.id)}" ${item.id === vm?.id ? "selected" : ""}>${esc(item.name)} · ${esc(item.id)}</option>`).join("")}</select>
      </label><div class="text-sm faint">${vm ? `所有操作固定到 ${esc(vm.id)}；需由管理器启动。` : "请选择虚拟机。"}</div></div></div>`;
  view.querySelector<HTMLSelectElement>("#device-page-vm")?.addEventListener("change", (event) => {
    state.currentVmId = (event.currentTarget as HTMLSelectElement).value || null;
    paintView();
  });
  return vm;
}

function renderApps(view: HTMLElement) {
  const vm = devicePageTarget(view, "应用管理", "查询、打开第三方应用，安装与卸载 APK；仅操作当前指定 VM");
  if (!vm) return;
  const vmId = vm.id; // Capture identity before any asynchronous dialog or request.
  const panel = document.createElement("div"); panel.className = "panel";
  panel.innerHTML = `<div class="panel-body"><div class="row" style="gap:8px">
    <input class="input" id="apps-apk" placeholder="本地 .apk 绝对路径" style="flex:1">
    <button class="btn" id="apps-install">安装 APK</button><button class="btn" id="apps-refresh">刷新应用</button>
    </div><pre id="apps-status" style="white-space:pre-wrap"></pre><div id="apps-list"></div></div>`;
  view.appendChild(panel);
  const status = panel.querySelector<HTMLElement>("#apps-status")!;
  const list = panel.querySelector<HTMLElement>("#apps-list")!;
  const output = (text: string, failure = false) => { status.textContent = text; status.style.color = failure ? "#C00000" : ""; };
  let busy = false;
  const setBusy = (value: boolean) => { busy = value; panel.querySelectorAll<HTMLButtonElement>("button").forEach((button) => button.disabled = value); };
  const refresh = async () => {
    const packages = await api.adbListPackages(vmId);
    list.replaceChildren();
    for (const name of packages) {
      const row = document.createElement("div"); row.className = "row"; row.style.marginTop = "8px";
      const label = document.createElement("span"); label.textContent = name; label.style.flex = "1";
      const uninstall = document.createElement("button"); uninstall.className = "btn btn-sm"; uninstall.textContent = "卸载";
      uninstall.addEventListener("click", async () => {
        if (busy || !window.confirm(`从 ${vmId} 卸载 ${name}？应用数据会随卸载移除。`)) return;
        setBusy(true);
        try {
          const result = await api.adbUninstall(vmId, name);
          if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
          await refresh(); output(`${vmId}: 已卸载 ${name}\n${result.stdout}`); log(`${vmId}: 已卸载 ${name}`, "ok");
        } catch (error) { output(`${vmId}: 卸载失败: ${error}`, true); log(`${vmId}: 卸载失败: ${error}`, "err"); }
        finally { setBusy(false); }
      });
      const launch = document.createElement("button"); launch.className = "btn btn-sm"; launch.textContent = "打开";
      launch.addEventListener("click", async () => {
        if (busy || !panel.isConnected) return;
        setBusy(true); output(`${vmId}: 正在打开 ${name}`);
        try {
          const result = await api.launchVmApp(vmId, name);
          if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
          if (panel.isConnected) output(`${vmId}: 已发送应用启动请求 ${name}\n${result.stdout}`);
          log(`${vmId}: 打开 ${name}；可手动刷新内部画面核对`, "info");
        } catch (error) { if (panel.isConnected) output(`${vmId}: 打开失败: ${error}`, true); }
        finally { setBusy(false); }
      });
      row.append(label, launch, uninstall); list.appendChild(row);
    }
    output(`${vmId}: 已连接 · ${packages.length} 个第三方应用`);
  };
  panel.querySelector("#apps-refresh")?.addEventListener("click", async () => {
    if (busy) return; setBusy(true);
    try { await refresh(); } catch(error) { output(`${vmId}: 查询失败: ${error}`, true); }
    finally { setBusy(false); }
  });
  panel.querySelector("#apps-install")?.addEventListener("click", async () => {
    if (busy) return;
    const path = panel.querySelector<HTMLInputElement>("#apps-apk")!.value.trim();
    if (!path.toLowerCase().endsWith(".apk")) { output("请填写本地 .apk 文件路径", true); return; }
    setBusy(true); output(`${vmId}: 安装中`);
    try {
      const result = await api.adbInstall(vmId, path);
      if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
      await refresh(); output(`${vmId}: APK 安装成功\n${result.stdout}`); log(`${vmId}: APK 安装成功`, "ok");
    } catch (error) { output(`${vmId}: 安装失败: ${error}`, true); log(`${vmId}: 安装失败: ${error}`, "err"); }
    finally { setBusy(false); }
  });
  view.appendChild(createGuestPreview(vmId));
  setBusy(true); output(`${vmId}: 查询中`);
  refresh().catch((error) => output(`${vmId}: 查询失败: ${error}`, true)).finally(() => setBusy(false));
}

function renderDiagnose(view: HTMLElement) {
  const vm = devicePageTarget(view, "崩溃诊断", "在指定虚拟机停止并重新启动所选应用，采集现有诊断报告");
  if (!vm) return;
  const vmId = vm.id;
  const panel = document.createElement("div"); panel.className = "panel";
  panel.innerHTML = `<div class="panel-body"><div class="row" style="gap:8px">
    <input class="input" id="diagnose-package" placeholder="应用包名，如 com.example.app" style="flex:1">
    <input class="input" id="diagnose-wait" type="number" min="1" max="120" value="30" style="width:90px" aria-label="观察秒数">
    <button class="btn" id="diagnose-run">运行诊断</button></div>
    <pre id="diagnose-result" style="white-space:pre-wrap;overflow-wrap:anywhere">尚未运行诊断</pre></div>`;
  view.appendChild(panel);
  const button = panel.querySelector<HTMLButtonElement>("#diagnose-run")!;
  const result = panel.querySelector<HTMLElement>("#diagnose-result")!;
  button.addEventListener("click", async () => {
    const name = panel.querySelector<HTMLInputElement>("#diagnose-package")!.value.trim();
    const seconds = Number(panel.querySelector<HTMLInputElement>("#diagnose-wait")!.value);
    if (!/^[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+)+$/.test(name) || !Number.isInteger(seconds) || seconds < 1 || seconds > 120) {
      result.textContent = "请填写有效包名与1–120秒观察时间"; return;
    }
    button.disabled = true; result.textContent = `${vmId}: 正在启动 ${name} 并采集诊断…`;
    try {
      const report = await api.diagnoseApp(vmId, name, seconds);
      result.textContent = `${vmId}: ${report.package}\n${report.timestamp}\n阶段：${report.phase_label}\n归因：${report.cause_label}\n置信度：${report.confidence}\n\n证据\n${report.evidence.join("\n")}\n\n建议\n${report.suggestions.join("\n")}\n\n匹配信号\n${JSON.stringify(report.matched_signals, null, 2)}\n\nCrash log\n${report.crash_log}\n\nTombstone\n${report.tombstone}`;
      log(`${vmId}: ${name} 诊断完成：${report.cause_label}`, "info");
    } catch (error) { result.textContent = `${vmId}: 诊断失败: ${error}`; log(`${vmId}: 诊断失败: ${error}`, "err"); }
    finally { button.disabled = false; }
  });
}

function renderCompat(view: HTMLElement) {
  view.innerHTML = `
    <div class="page-head">
      <div><h1>兼容性矩阵</h1><p>每测试一个应用就累积一条记录</p></div>
      <div class="spacer"></div>
      <button class="btn btn-sm" id="btn-csv">${icon("download", 13)} 导出 CSV</button>
    </div>
    <div class="panel" style="flex:1">
      <div class="panel-body" id="compat-body"></div>
    </div>
  `;

  view.querySelector("#btn-csv")?.addEventListener("click", async () => {
    try {
      const p = await api.exportCompatCsv();
      log(`已导出: ${p}`, "ok");
    } catch (e) {
      log(`导出失败: ${e}`, "err");
    }
  });

  loadCompat();
}

async function loadCompat() {
  const body = document.querySelector("#compat-body");
  if (!body) return;

  try {
    const rows = await api.listCompat();
    if (!rows.length) {
      body.innerHTML = `<div class="empty">
        ${icon("list", 34)}
        <div class="t">矩阵为空</div>
        <div class="d">测试应用后会在此累积记录</div>
      </div>`;
      return;
    }

    body.innerHTML = `
      <div style="display:grid;grid-template-columns:1fr 110px 80px 60px;gap:12px;
                  padding:6px 4px;font-size:10px;font-weight:650;letter-spacing:0.5px;
                  color:var(--fg-faint);text-transform:uppercase;
                  border-bottom:1px solid var(--line)">
        <span>包名</span><span>状态</span><span>Root</span><span style="text-align:right">次数</span>
      </div>
      ${rows
        .map(
          (r) => `
        <div style="display:grid;grid-template-columns:1fr 110px 80px 60px;gap:12px;
                    padding:10px 4px;border-bottom:1px solid var(--line-soft);align-items:center">
          <span class="mono text-sm truncate">${esc(r.package)}</span>
          <span class="chip ${stateChip(r.state)}">${esc(r.state)}</span>
          <span class="text-sm faint">${esc(r.root_mode)}</span>
          <span class="text-sm faint" style="text-align:right">${r.test_count}</span>
        </div>`
        )
        .join("")}
    `;
  } catch (e) {
    body.innerHTML = `<div class="empty"><div class="t">读取失败</div></div>`;
  }
}

const stateChip = (s: string) =>
  s === "正常" ? "ok" : s.includes("崩") ? "err" : s.includes("部分") ? "warn" : "plain";

function renderSettings(view: HTMLElement) {
  const e = state.env;
  const rows: [string, string, boolean][] = [
    ["项目根目录", e?.project_root ?? "—", !!e],
    ["QEMU", e?.qemu ? "已就绪" : "缺失", !!e?.qemu],
    ["qemu-img", e?.qemu_img ? "已就绪" : "缺失", !!e?.qemu_img],
    ["adb", e?.adb ? "已就绪" : "缺失", !!e?.adb],
    ["scrcpy", e?.scrcpy ? "已就绪" : "未安装", !!e?.scrcpy],
    ["基础镜像", e?.images_ready ? "已就绪" : `缺失 ${e?.missing_images.length ?? 0} 项`, !!e?.images_ready],
  ];

  view.innerHTML = `
    <div class="page-head">
      <div><h1>设置</h1><p>运行时状态与产品信息</p></div>
    </div>

    <div class="panel">
      <div class="panel-head"><div class="titles"><div class="t">运行时</div><div class="s">绿色便携，卸载即删除目录</div></div></div>
      <div class="panel-body">
        ${rows
          .map(
            ([k, v, ok]) => `
          <div class="row" style="padding:9px 0;border-bottom:1px solid var(--line-soft)">
            <span class="text-sm dim" style="width:110px;flex-shrink:0">${k}</span>
            <span class="text-sm faint mono truncate" style="flex:1">${esc(v)}</span>
            <span class="rail-dot ${ok ? "ok" : "warn"}"></span>
          </div>`
          )
          .join("")}
      </div>
    </div>

    <div class="panel">
      <div class="panel-head"><div class="titles"><div class="t">关于</div></div></div>
      <div class="panel-body">
        <div class="row" style="gap:16px;padding-bottom:16px;border-bottom:1px solid var(--line-soft);margin-bottom:14px">
          <div style="width:56px;height:56px">${roostIcon({ size: 56 })}</div>
          <div>
            <div style="font-size:17px;font-weight:700;letter-spacing:-0.3px">Roost</div>
            <div class="text-sm faint">Android Container Manager</div>
            <div class="text-xs faint" style="margin-top:2px">v0.1.0</div>
          </div>
        </div>
        ${[
          ["技术栈", "Tauri 2 + Rust + TypeScript"],
          ["加速方式", "Android Emulator + 已安装的 AEHD；旧 QEMU 配置保留"],
          ["系统影响", "应用不自动修改主机驱动或虚拟化配置"],
          ["Root 切换", "停机选择匹配镜像的 prepared ramdisk，再冷启动核验"],
        ]
          .map(
            ([k, v]) => `
          <div class="row" style="padding:9px 0;border-bottom:1px solid var(--line-soft)">
            <span class="text-sm dim" style="width:110px;flex-shrink:0">${k}</span>
            <span class="text-sm faint">${esc(v)}</span>
          </div>`
          )
          .join("")}
      </div>
    </div>
  `;
}

// ------------------------------------------------------------------ 动作

async function createVm() {
  const name = prompt("虚拟机名称", "新虚拟机");
  if (!name) return;

  const id = "vm" + (state.vms.length + 1) + "_" + Date.now().toString(36).slice(-4);
  try {
    const vm = await api.createVm(id, name);
    state.vms = await api.listVms();
    state.currentVmId = vm.id;
    log(`已创建: ${name}`, "ok");
    paintView();
  } catch (e) {
    log(`创建失败: ${e}`, "err");
  }
}

async function deleteVm() {
  if (!state.currentVmId) return;
  if (!confirm("删除此虚拟机？磁盘数据将一并删除。")) return;

  try {
    await api.deleteVm(state.currentVmId);
    state.vms = await api.listVms();
    state.currentVmId = state.vms[0]?.id ?? null;
    log("已删除", "ok");
    paintView();
  } catch (e) {
    log(`删除失败: ${e}`, "err");
  }
}

async function startVm() {
  const vm = state.vms.find((v) => v.id === state.currentVmId);
  if (!vm || state.starting.has(vm.id) || state.running.has(vm.id)) return;
  const session = Symbol(vm.id);
  const headless = state.headless;
  state.bootSessions.set(vm.id, session);
  state.starting.add(vm.id);
  const active = () => state.bootSessions.get(vm.id) === session;
  paintView();
  log(`启动 ${vm.name}（${headless ? "无窗口后台" : "模拟器窗口"}）...`);
  try {
    const r = await api.launchVm(vm.id, headless);
    if (!active()) return;
    if (!r.ok) { log(r.message, "err"); return; }
    state.running.add(vm.id);
    log(`${r.backend} 进程已创建 (PID ${r.pid}) · ${r.serial}；Android 尚待启动核验`, "info");
    const launchedVms = await api.listVms();
    if (!active()) return;
    state.vms = launchedVms;
    paintView();
    const deadline = Date.now() + 240_000;
    let connected = false;
    let lastWait = "等待指定设备上线";
    while (Date.now() < deadline && active()) {
      await new Promise((resolve) => setTimeout(resolve, 4000));
      if (!active()) return;
      const running = new Set(await api.runningVms());
      if (!active()) return;
      state.running = running;
      if (!running.has(vm.id)) { log(`${vm.name} 进程已退出；请查看启动日志`, "err"); return; }
      try {
        if (!await api.adbConnect(vm.id)) continue;
        if (!active()) return;
        if (!connected) { log(`ADB 已定向连接 ${r.serial}；不等同于系统已启动`, "info"); connected = true; }
        const boot = await api.vmBootStatus(vm.id);
        if (!active()) return;
        if (boot.serial !== r.serial) throw new Error("启动核验返回了不同设备身份");
        if (!boot.boot_completed) { lastWait = "指定设备已连接，等待sys.boot_completed=1"; continue; }
        const info = await api.adbDeviceInfo(vm.id);
        if (!active()) return;
        if (!info.connected || info.serial !== r.serial) { lastWait = "启动属性读取未确认指定设备"; continue; }
        log(`Android 启动核验完成 · ${info.model} · API ${info.sdk} · ${info.abi}`, "ok");
        log("Root/LSPosed尚未在本次启动核验；如需Root，请在内部画面点击“请求Shell Root”，明确授权后再点版本验证", "info");
        const readyVms = await api.listVms();
        if (!active()) return;
        state.vms = readyVms;
        paintView();
        return;
      } catch (e) { lastWait = String(e); }
    }
    if (active()) log(`Android 启动核验超时：${lastWait}；进程运行状态不作为启动成功`, "warn");
  } catch (e) {
    if (active()) log(`启动失败: ${e}`, "err");
  } finally {
    if (active()) {
      state.bootSessions.delete(vm.id);
      state.starting.delete(vm.id);
      paintView();
    }
  }
}

async function stopVm() {
  const vm = state.vms.find((v) => v.id === state.currentVmId);
  if (!vm) return;

  state.bootSessions.delete(vm.id);
  state.starting.delete(vm.id);
  try {
    const msg = await api.stopVm(vm.id);
    state.running.delete(vm.id);
    log(msg, "ok");
    paintView();
  } catch (e) {
    log(`停止失败: ${e}`, "err");
  }
}

// ------------------------------------------------------------------ 启动

async function init() {
  paintShell();
  paintRail();
  paintView();

  try {
    state.env = await api.envStatus();
    paintRail();
    log("环境检查完成", "ok");
    if (!state.env.images_ready) {
      log(`基础镜像缺失 ${state.env.missing_images.length} 项`, "warn");
    }
  } catch (e) {
    log(`后端连接失败: ${e}`, "err");
  }

  // Local VM controls must not depend on GitHub or mirror availability.
  try {
    state.vms = await api.listVms();
    state.currentVmId = state.vms[0]?.id ?? null;
    state.running = new Set(await api.runningVms());
    log(`已载入 ${state.vms.length} 个本地虚拟机；在线版本列表后台刷新`, "ok");
    paintView();
  } catch (e) {
    log(`本地虚拟机加载失败: ${e}`, "err");
  }
  await Promise.allSettled([
    (async () => { state.overlays = await api.listOverlays(); paintView(); })(),
    (async () => { await loadManagers(); paintView(); })(),
    (async () => { state.lsp = await api.listLsposed(); paintView(); })(),
    (async () => { state.aospImages = await api.listAospImages(); paintView(); })(),
  ]);
}

init();
