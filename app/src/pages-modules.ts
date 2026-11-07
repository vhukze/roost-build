/**
 * LSP 管理页 与 模块管理页
 *
 * LSP 页：LSPosed 版本下载 + 模块仓库
 * 模块页：Magisk 模块仓库 + 本地文件安装
 */

import {
  api,
  type PkgManager,
  type PkgVersion,
  type ModuleInfo,
  type VmConfig,
} from "./api";
import { icons } from "./icons";

// ------------------------------------------------------------------ 工具

const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

const icon = (name: keyof typeof icons, size = 16) =>
  icons[name].replace("<svg ", `<svg width="${size}" height="${size}" `);

// ------------------------------------------------------------------ 共享状态

export interface LspState {
  lsp: PkgManager | null;
  lspLoading: boolean;
  modules: ModuleInfo[];
  moduleResults: Record<"lsposed" | "magisk", ModuleInfo[]>;
  moduleQueries: Record<"lsposed" | "magisk", string>;
  moduleBusy: Record<"lsposed" | "magisk", boolean>;
  modulesLoading: boolean;
  moduleQuery: string;
  moduleOrg: "lsposed" | "magisk";
  modulesPage: number;
  vms: VmConfig[];
  currentVmId: string | null;
  log: (msg: string, level?: "info" | "ok" | "warn" | "err") => void;
}

// ------------------------------------------------------------------ LSP 页

export function renderLspPage(
  view: HTMLElement,
  state: LspState,
  rerender: () => void
) {
  const lsp = state.lsp;

  if (state.lspLoading) {
    view.innerHTML = `
      <div class="page-head"><div><h1>LSP 管理</h1><p>正在查询 LSPosed 版本...</p></div></div>
      <div class="panel" style="flex:1"><div class="empty">
        ${icon("refresh", 34)}
        <div class="t">正在加载</div>
        <div class="d">从 GitHub 获取 LSPosed 版本清单</div>
      </div></div>`;
    return;
  }

  const downloadedVersions = (lsp?.versions ?? []).filter((v) => v.downloaded);
  const currentVm = state.vms.find((v) => v.id === state.currentVmId);

  view.innerHTML = `
    <div class="page-head">
      <div><h1>LSP 管理</h1><p>LSPosed 框架版本与模块仓库</p></div>
      <div class="spacer"></div>
      <button class="btn btn-sm" id="lsp-refresh">${icon("refresh", 13)} 刷新</button>
    </div>

    ${
      lsp && lsp.status !== "ok"
        ? `<div class="panel" style="border-color:rgba(245,165,36,.3)">
             <div class="panel-body" style="padding:14px 18px;display:flex;gap:10px;align-items:center">
               ${icon("alert", 16)}
               <span class="text-sm" style="color:var(--warn)">版本查询失败：${esc(lsp.status)}</span>
             </div>
           </div>`
        : ""
    }

    <div class="stats" style="grid-template-columns:repeat(3,1fr)">
      <div class="stat">
        <div class="k">可选版本</div>
        <div class="v">${lsp?.versions.length ?? 0}</div>
      </div>
      <div class="stat">
        <div class="k">已下载</div>
        <div class="v">${downloadedVersions.length}</div>
      </div>
      <div class="stat">
        <div class="k">当前虚拟机</div>
        <div class="v" style="font-size:13px">${currentVm ? esc(currentVm.name) : "未选择"}</div>
      </div>
    </div>

    <!-- LSPosed 版本 -->
    <div class="panel">
      <div class="panel-head">
        <div class="titles">
          <div class="t">LSPosed 版本</div>
          <div class="s">每行可选 Zygisk / Riru 两种变体，Zygisk 需 Root 方案支持</div>
        </div>
      </div>
      <div style="display:flex;flex-direction:column;padding:12px 20px 16px;gap:8px;
                  max-height:560px;overflow-y:auto">
        ${
          (lsp?.versions ?? []).length === 0
            ? `<div class="text-sm faint" style="padding:12px 0">暂无版本数据</div>`
            : groupByVersion(lsp!.versions)
                .map(([ver, items]) => lspVersionRow(ver, items, state))
                .join("")
        }
      </div>
    </div>

    <!-- 模块仓库（内嵌） -->
    <div id="lsp-modules-host"></div>
  `;

  view.querySelector("#lsp-refresh")?.addEventListener("click", async () => {
    state.lspLoading = true;
    rerender();
    try {
      state.lsp = await api.listLsposed();
      state.log("LSPosed 版本已刷新", "ok");
    } catch (e) {
      state.log(`刷新失败: ${e}`, "err");
    } finally {
      state.lspLoading = false;
      rerender();
    }
  });

  // 下载按钮
  view.querySelectorAll<HTMLButtonElement>("[data-lsp-dl]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const [ver, variant, mirror] = btn.dataset.lspDl!.split("|");
      await downloadLsp(ver, variant, mirror === "1", btn, state, rerender);
    });
  });

  // 删除按钮
  view.querySelectorAll<HTMLButtonElement>("[data-lsp-rm]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const [ver, variant] = btn.dataset.lspRm!.split("|");
      if (!confirm(`删除 LSPosed ${ver} (${variant})？`)) return;
      try {
        await api.removeLsposed(ver, variant);
        state.lsp = await api.listLsposed();
        state.log(`已删除 LSPosed ${ver} ${variant}`, "ok");
        rerender();
      } catch (e) {
        state.log(`删除失败: ${e}`, "err");
      }
    });
  });

  // 打开 TG 消息
  view.querySelectorAll<HTMLButtonElement>("[data-open-tg]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const url = btn.dataset.openTg!;
      try {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(url);
        state.log(`已在浏览器打开: ${url}`, "ok");
      } catch (e) {
        state.log(`打开失败: ${e}`, "err");
      }
    });
  });

  // 挂载模块仓库
  const host = view.querySelector<HTMLElement>("#lsp-modules-host");
  if (host) {
    renderModuleSection(host, "lsposed", state, rerender);
  }
}

function groupByVersion(versions: PkgVersion[]): [string, PkgVersion[]][] {
  const map = new Map<string, PkgVersion[]>();
  for (const v of versions) {
    if (!map.has(v.version)) map.set(v.version, []);
    map.get(v.version)!.push(v);
  }
  return [...map.entries()];
}

function lspVersionRow(ver: string, items: PkgVersion[], state: LspState): string {
  const anyDownloaded = items.some((i) => i.downloaded);
  const featured = items.some((i) => i.featured);
  const date = items[0]?.date ?? "";

  return `
    <div style="padding:10px 12px;background:var(--bg-input);border:1px solid var(--line-soft);
                border-radius:var(--r-sm);display:flex;flex-direction:column;gap:8px">
      <div class="row" style="gap:10px">
        <span class="mono text-sm" style="font-weight:600">v${esc(ver)}</span>
        ${featured ? `<span class="chip info" style="font-size:9px">推荐</span>` : ""}
        ${anyDownloaded ? `<span class="chip ok"><span class="dot"></span>已下载</span>` : ""}
        <span class="text-xs faint">${esc(date)}</span>
        <div class="spacer"></div>
      </div>
      ${items
        .map((v) => {
          const label =
            v.variant === "zygisk"
              ? "Zygisk"
              : v.variant === "riru"
                ? "Riru"
                : v.variant === "unified"
                  ? "完整包"
                  : v.variant === "tg"
                    ? "TG 发布"
                    : v.variant;
          const badge =
            v.variant === "zygisk"
              ? "ok"
              : v.variant === "unified"
                ? "info"
                : v.variant === "tg"
                  ? "warn"
                  : "plain";

          const isTg = v.variant === "tg";

          return `
        <div class="row" style="gap:8px;padding-left:8px">
          <span class="chip ${badge}" style="min-width:62px;justify-content:center">${label}</span>
          <span class="text-xs faint" style="min-width:64px">${esc(v.size)}</span>
          ${
            v.variant === "unified"
              ? `<span class="text-xs faint">含 framework + manager + lspctl</span>`
              : isTg
                ? `<span class="text-xs faint">需在 Telegram 客户端下载</span>`
                : ""
          }
          <div class="spacer"></div>
          ${
            v.downloaded
              ? `<button class="btn btn-ghost btn-sm" data-lsp-rm="${ver}|${v.variant}"
                         style="color:var(--fg-faint)">删除</button>`
              : isTg
                ? `<button class="btn btn-sm" data-open-tg="${esc(v.url)}">
                     ${icon("opener", 12)} 打开 TG
                   </button>`
                : `<button class="btn btn-sm" data-lsp-dl="${ver}|${v.variant}|0">直连</button>
                   <button class="btn btn-sm" data-lsp-dl="${ver}|${v.variant}|1">镜像</button>`
          }
        </div>`;
        })
        .join("")}
    </div>`;
}

async function downloadLsp(
  version: string,
  variant: string,
  useMirror: boolean,
  btn: HTMLButtonElement,
  state: LspState,
  rerender: () => void
) {
  const items = state.lsp?.versions.filter(
    (v) => v.version === version && v.variant === variant
  );
  const target = items?.[0];
  if (!target) return;

  btn.disabled = true;
  btn.textContent = "下载中";

  try {
    await api.downloadLsposed(
      version,
      variant,
      target.url,
      target.asset_name,
      useMirror
    );
    state.log(`LSPosed ${version} (${variant}) 下载完成`, "ok");
    state.lsp = await api.listLsposed();
    rerender();
  } catch (e) {
    state.log(`LSPosed 下载失败: ${String(e)}`, "err");
    btn.disabled = false;
    btn.textContent = "重试";
  }
}

// ------------------------------------------------------------------ 模块段（两页共用）

export function renderModuleSection(
  host: HTMLElement,
  org: "lsposed" | "magisk",
  state: LspState,
  rerender: () => void
) {
  state.moduleOrg = org;
  state.modules = state.moduleResults[org];
  state.moduleQuery = state.moduleQueries[org];
  state.modulesLoading = state.moduleBusy[org];
  const title = org === "magisk" ? "Magisk 模块仓库" : "LSPosed 模块仓库";
  const subtitle =
    org === "magisk"
      ? "Magisk-Modules-Repo 组织的模块，可下载后安装到虚拟机"
      : "Xposed-Modules-Repo 组织的模块，即 modules.lsposed.org 的后端数据";

  host.innerHTML = `
    <div class="panel">
      <div class="panel-head">
        <div class="titles"><div class="t">${title}</div><div class="s">${subtitle}</div></div>
        <div class="row" style="gap:6px">
          <input class="input" id="mod-q" placeholder="搜索模块名或包名"
                 value="${esc(state.moduleQuery)}"
                 style="width:220px;height:28px;font-size:12px">
          <button class="btn btn-sm" id="mod-search">${icon("search", 12)} 搜索</button>
        </div>
      </div>
      <div style="padding:12px 20px 16px">
        <div id="mod-list" style="display:flex;flex-direction:column;gap:6px">
          ${
            state.modulesLoading
              ? `<div class="empty" style="padding:32px">
                   ${icon("refresh", 28)}
                   <div class="t">加载中</div>
                 </div>`
              : state.modules.length === 0
                ? `<div class="empty" style="padding:32px">
                     ${icon("package", 28)}
                     <div class="t">暂无模块</div>
                     <div class="d">输入关键词搜索，或留空浏览全部</div>
                   </div>`
                : state.modules.map((m) => moduleRow(m, state)).join("")
          }
        </div>
      </div>
    </div>
  `;

  const doSearch = async (page = 1) => {
    state.moduleQuery =
      host.querySelector<HTMLInputElement>("#mod-q")?.value ?? "";
    state.moduleQueries[org] = state.moduleQuery;
    state.moduleBusy[org] = true;
    rerender();
    try {
      state.moduleResults[org] = await api.searchModules(org, state.moduleQueries[org], page);
      state.log(`找到 ${state.moduleResults[org].length} 个模块`, "ok");
    } catch (e) {
      state.log(`模块搜索失败: ${e}`, "err");
      state.moduleResults[org] = [];
    } finally {
      state.moduleBusy[org] = false;
      state.modules = state.moduleResults[org];
      state.modulesLoading = false;
      rerender();
    }
  };

  host.querySelector("#mod-search")?.addEventListener("click", () => doSearch(1));
  host.querySelector<HTMLInputElement>("#mod-q")?.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") doSearch(1);
  });

  // 安装按钮
  host.querySelectorAll<HTMLButtonElement>("[data-mod-install]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const fullName = btn.dataset.modInstall!;
      const module = state.moduleResults[org].find((item) => item.full_name === fullName);
      if (!module || (module.kind !== "apk" && module.kind !== "magisk_zip")) { state.log("模块资产类型缺失，请重新搜索", "err"); return; }
      await installModule(fullName, module.kind, btn, state, rerender);
    });
  });
}

function moduleRow(m: ModuleInfo, state: LspState): string {
  return `
    <div class="row" style="padding:9px 12px;background:var(--bg-input);
                border:1px solid var(--line-soft);border-radius:var(--r-sm);gap:12px">
      <div class="col" style="flex:1;min-width:0;gap:2px">
        <div class="row" style="gap:8px">
          <span class="text-sm mono" style="font-weight:600">${esc(m.package)}</span>
          ${m.stars > 0 ? `<span class="text-xs faint">★ ${m.stars}</span>` : ""}
        </div>
        <span class="text-xs faint truncate">${esc(m.description || "无描述")}</span>
      </div>
      <span class="text-xs faint" style="min-width:80px">${esc(m.updated)}</span>
      <button class="btn btn-sm" data-mod-install="${esc(m.full_name)}">
        ${icon("download", 12)} 获取
      </button>
    </div>
  `;
}

async function installModule(
  fullName: string,
  kind: "apk" | "magisk_zip",
  btn: HTMLButtonElement,
  state: LspState,
  rerender: () => void
) {
  btn.disabled = true;
  btn.textContent = "查询中";
  const vm = state.vms.find((v) => v.id === state.currentVmId);
  if (!vm) { state.log("请先选择目标虚拟机", "warn"); btn.disabled = false; btn.textContent = "获取"; return; }

  try {
    // 1. Resolve a release asset of the requested kind.
    const rel = await api.moduleRelease(fullName, kind);
    btn.textContent = "下载中";

    // 2. Download selected APK or ZIP asset.
    const path = await api.downloadModule(
      fullName,
      rel.apk_url,
      rel.apk_name,
      false
    );

    state.log(`模块已下载: ${rel.apk_name} → ${path}`, "ok");

    // Target VM captured before download; changing the UI selection cannot retarget installation.
    btn.textContent = "安装中";
    const result = kind === "magisk_zip" ? await api.installMagiskModule(vm.id, path) : await api.adbInstall(vm.id, path);
    if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
    state.log(`${vm.name}: ${kind === "magisk_zip" ? "Magisk ZIP 已安装，待该VM重启" : "APK 已安装"}`, "ok");
    btn.innerHTML = `${icon("check", 12)} 完成`;
    state.vms = await api.listVms();
    rerender();
  } catch (e) {
    state.log(`模块安装失败: ${String(e)}`, "err");
    btn.disabled = false;
    btn.innerHTML = `${icon("download", 12)} 重试`;
  }
}

// ------------------------------------------------------------------ Magisk 模块页

export function renderMagiskModulesPage(
  view: HTMLElement,
  state: LspState,
  rerender: () => void
) {
  view.innerHTML = `
    <div class="page-head">
      <div><h1>Magisk 模块</h1><p>从仓库下载模块，或上传本地模块文件安装</p></div>
    </div>

    <!-- 本地安装 -->
    <div class="panel">
      <div class="panel-head">
        <div class="titles">
          <div class="t">本地安装</div>
          <div class="s">选择 .zip 模块包或 .apk 模块，安装到当前虚拟机</div>
        </div>
      </div>
      <div class="panel-body">
        <div class="row" style="gap:10px">
          <input class="input" id="local-path" placeholder="模块文件路径（.zip / .apk）"
                 style="flex:1;height:34px">
          <button class="btn btn-accent" id="btn-local-install">
            ${icon("download", 13)} 安装到虚拟机
          </button>
        </div>
        <div class="text-xs faint" style="margin-top:8px">
          当前虚拟机：${(() => {
            const vm = state.vms.find((v) => v.id === state.currentVmId);
            return vm ? esc(vm.name) : "未选择";
          })()}
        </div>
      </div>
    </div>

    <div id="magisk-mod-host"></div>
  `;

  view.querySelector("#btn-local-install")?.addEventListener("click", async () => {
    const path = view.querySelector<HTMLInputElement>("#local-path")?.value.trim();
    if (!path) {
      state.log("请填写模块文件路径", "warn");
      return;
    }
    const vm = state.vms.find((v) => v.id === state.currentVmId);
    if (!vm) {
      state.log("未选择虚拟机", "warn");
      return;
    }
    try {
      const isZip = path.toLowerCase().endsWith(".zip");
      if (!isZip && !path.toLowerCase().endsWith(".apk")) throw new Error("请选择 .zip 或 .apk 文件");
      const result = isZip ? await api.installMagiskModule(vm.id, path) : await api.adbInstall(vm.id, path);
      if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
      state.log(`${vm.name}: ${isZip ? "Magisk ZIP 已安装，待该VM重启" : "APK 已安装"}: ${path}`, "ok");
      state.vms = await api.listVms();
      rerender();
    } catch (e) {
      state.log(`安装失败: ${String(e)}`, "err");
    }
  });

  const host = view.querySelector<HTMLElement>("#magisk-mod-host");
  if (host) {
    renderModuleSection(host, "magisk", state, rerender);
  }
}
