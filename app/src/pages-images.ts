/**
 * 系统镜像页
 *
 * 从 Google 官方仓库下载预编译 arm64 AOSP 镜像，
 * 无需本地编译（省去 250 GB 磁盘与数小时构建）。
 */

import { api, type ImageEntry, type ImageProgress } from "./api";
import { icons } from "./icons";

const icon = (name: keyof typeof icons, size = 16) =>
  icons[name].replace("<svg ", `<svg width="${size}" height="${size}" `);

const esc = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

export interface ImageState {
  images: ImageEntry[];
  loading: boolean;
  progress: ImageProgress | null;
  log: (msg: string, level?: "info" | "ok" | "warn" | "err") => void;
  onDone: () => void;
}

export function renderImagesPage(
  view: HTMLElement,
  state: ImageState,
  rerender: () => void
) {
  if (state.loading) {
    view.innerHTML = `
      <div class="page-head"><div><h1>系统镜像</h1><p>正在查询可用镜像...</p></div></div>
      <div class="panel" style="flex:1"><div class="empty">
        ${icon("refresh", 32)}
        <div class="t">正在加载</div>
        <div class="d">从 dl.google.com 获取 arm64 镜像清单</div>
      </div></div>`;
    return;
  }

  const installed = state.images.filter((i) => i.downloaded);
  const p = state.progress;

  view.innerHTML = `
    <div class="page-head">
      <div>
        <h1>系统镜像</h1>
        <p>Google 官方预编译 arm64 镜像 · 无需本地编译 AOSP</p>
      </div>
      <div class="spacer"></div>
      <button class="btn btn-sm" id="img-refresh">${icon("refresh", 13)} 刷新</button>
    </div>

    ${
      p && p.stage !== "done" && p.stage !== "error"
        ? `<div class="panel" style="border-color:rgba(46,229,148,.35)">
             <div class="panel-body" style="padding:14px 18px">
               <div class="row" style="justify-content:space-between;margin-bottom:8px">
                 <span class="text-sm" style="color:var(--green)">
                   API ${p.api} · ${esc(p.stage === "downloading" ? "下载中" : p.stage === "extracting" ? "解压中" : "转换中")}
                 </span>
                 <span class="text-xs faint">${esc(p.message)}</span>
               </div>
               <div style="height:6px;background:var(--bg-raised);border-radius:3px;overflow:hidden">
                 <div style="height:100%;width:${Math.min(100, p.percent)}%;background:var(--green);
                             transition:width .3s"></div>
               </div>
               <div class="text-xs faint" style="margin-top:6px">${p.percent.toFixed(1)}%</div>
             </div>
           </div>`
        : ""
    }

    <div class="stats" style="grid-template-columns:repeat(3,1fr)">
      <div class="stat">
        <div class="k">可用镜像</div>
        <div class="v">${state.images.length}</div>
      </div>
      <div class="stat">
        <div class="k">已安装</div>
        <div class="v">${installed.length}</div>
      </div>
      <div class="stat">
        <div class="k">当前状态</div>
        <div class="v" style="font-size:13px;color:${installed.length ? "var(--ok)" : "var(--warn)"}">
          ${installed.length ? "已就绪" : "未安装"}
        </div>
      </div>
    </div>

    <div class="panel">
      <div class="panel-head">
        <div class="titles">
          <div class="t">可用镜像</div>
          <div class="s">选择与目标 App 的 minSdk 匹配的版本，越新兼容性越好但镜像越大</div>
        </div>
      </div>
      <div style="display:flex;flex-direction:column;padding:12px 20px 16px;gap:8px;
                  max-height:520px;overflow-y:auto">
        ${state.images
          .map((img) => {
            return `
          <div class="row" style="padding:10px 14px;background:var(--bg-input);
                      border:1px solid ${img.downloaded ? "rgba(46,229,148,.3)" : "var(--line-soft)"};
                      border-radius:var(--r-sm);gap:14px">
            <div class="col" style="min-width:104px;gap:2px">
              <span class="text-sm mono" style="font-weight:600">API ${img.api}</span>
              <span class="text-xs faint">${esc(img.android_version)}</span>
            </div>
            <div class="col" style="flex:1;min-width:0;gap:2px">
              <span class="text-xs faint">${esc(img.abi)} · ${esc(img.size)}</span>
              <span class="text-xs faint">${esc(img.note)}</span>
            </div>
            ${
              img.downloaded
                ? `<span class="chip ok"><span class="dot"></span>已安装</span>
                   <button class="btn btn-ghost btn-sm" data-img-rm="${img.api}"
                           style="color:var(--fg-faint)">删除</button>`
                : `<button class="btn btn-sm btn-accent" data-img-dl="${img.api}">
                     ${icon("download", 12)} 下载安装
                   </button>`
            }
          </div>`;
          })
          .join("")}
      </div>
    </div>

    <div class="panel">
      <div class="panel-head"><div class="titles"><div class="t">为什么用预编译镜像</div></div></div>
      <div class="panel-body">
        <div class="row" style="gap:20px;flex-wrap:wrap">
          <div class="col" style="gap:3px;min-width:150px">
            <span class="text-xs faint">自行编译 AOSP</span>
            <span class="text-sm">250 GB 磁盘 · 3~6 小时</span>
          </div>
          <div class="col" style="gap:3px;min-width:150px">
            <span class="text-xs faint">下载预编译镜像</span>
            <span class="text-sm" style="color:var(--green)">1.5 GB · 约 5 分钟</span>
          </div>
        </div>
      </div>
    </div>
  `;

  view.querySelector("#img-refresh")?.addEventListener("click", async () => {
    state.loading = true;
    rerender();
    try {
      state.images = await api.listAospImages();
      state.log("镜像清单已刷新", "ok");
    } catch (e) {
      state.log(`刷新失败: ${e}`, "err");
    } finally {
      state.loading = false;
      rerender();
    }
  });

  view.querySelectorAll<HTMLButtonElement>("[data-img-dl]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const apiNum = Number(btn.dataset.imgDl);
      const target = state.images.find((i) => i.api === apiNum);
      if (!target) return;

      btn.disabled = true;
      btn.textContent = "准备中";

      // 监听进度
      const { listen } = await import("@tauri-apps/api/event");
      const unlisten = await listen<ImageProgress>("aosp-image-progress", (ev) => {
        state.progress = ev.payload;
        rerender();
      });

      try {
        await api.downloadAospImage(target.api, target.url);
        state.log(`API ${target.api} 镜像安装完成`, "ok");
        state.images = await api.listAospImages();
        state.progress = null;
        state.onDone();
        rerender();
      } catch (e) {
        state.log(`下载失败: ${String(e)}`, "err");
        state.progress = null;
        btn.disabled = false;
        btn.innerHTML = `${icon("download", 12)} 重试`;
      } finally {
        unlisten();
      }
    });
  });

  view.querySelectorAll<HTMLButtonElement>("[data-img-rm]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const apiNum = Number(btn.dataset.imgRm);
      if (!confirm(`删除 API ${apiNum} 镜像？已运行的虚拟机会受影响。`)) return;
      try {
        await api.removeAospImage(apiNum);
        state.images = await api.listAospImages();
        state.log(`已删除 API ${apiNum}`, "ok");
        state.onDone();
        rerender();
      } catch (e) {
        state.log(`删除失败: ${e}`, "err");
      }
    });
  });
}
