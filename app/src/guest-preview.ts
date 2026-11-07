
import { api, type CaptureVmGuestResult, type CmdResult } from "./api";
import { mapGuestPoint } from "./guest-coordinates.mjs";
import { createRequestGate } from "./guest-request-gate.mjs";

const FRAME_INPUT_MAX_AGE_MS = 30_000;
const BOOT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
// Renderer-wide per-VM request identity survives page repaint/navigation.
const rootRequests = createRequestGate<CmdResult>();

function requestRootOnce(vmId: string): Promise<CmdResult> {
  return rootRequests.start(vmId, () => api.requestVmRoot(vmId));
}

/** Guest-only manual screenshot and opt-in one-shot taps. No host input or timers. */
export function createGuestPreview(vmId: string): HTMLElement {
  const panel = document.createElement("section");
  panel.className = "panel";
  panel.innerHTML = `<div class="panel-head"><div class="titles"><div class="t">虚拟机内部画面</div><div class="s">手动截图；点击只发送到此 VM，不读取桌面、不操作宿主焦点</div></div><button class="btn btn-sm" type="button">刷新画面</button></div><div class="panel-body"><div style="display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-bottom:10px"><button class="btn btn-sm" type="button" data-root>请求Shell Root（手动授权）</button><span class="text-xs faint" data-root-status>未发起Root请求；启动和截图不主动请求su。</span></div><p class="text-xs faint">先打开Magisk管理器，再点“请求Shell Root”。等待期间可继续刷新截图并手动选择授权；启动预检后Root命令最多等待90秒，不自动授权、不改策略。请求结束不代表持久Root或LSPosed已通过验收。</p><p class="text-xs faint" data-status></p><label class="text-xs faint" style="display:flex;align-items:center;gap:6px;margin-bottom:10px"><input type="checkbox" data-input disabled>启用本张画面的一次点击（默认关闭）</label><p class="text-xs faint">需先刷新；30秒内可启用一次点击。用于你手动操作 Android，包括明确选择 Magisk 授权；应用不会自动选择授权。每次点击后需重新刷新。</p><img alt="指定虚拟机的内部截图" hidden draggable="false" style="max-width:100%;max-height:560px;object-fit:contain;object-position:center;padding:0;border:0;display:none"><p class="text-xs faint" data-evidence style="overflow-wrap:anywhere"></p></div>`;
  const button = panel.querySelector<HTMLButtonElement>("button")!;
  const input = panel.querySelector<HTMLInputElement>("[data-input]")!;
  const status = panel.querySelector<HTMLElement>("[data-status]")!;
  const image = panel.querySelector<HTMLImageElement>("img")!;
  const evidence = panel.querySelector<HTMLElement>("[data-evidence]")!;
  let busy = false;
  let frame: CaptureVmGuestResult | null = null;
  let frameStartedAt = 0;
  const disarm = () => { input.checked = false; input.disabled = true; image.style.cursor = "default"; };
  const clearFrame = () => {
    frame = null; disarm(); image.hidden = true; image.style.display = "none";
    image.removeAttribute("src"); evidence.textContent = "";
  };
  const rootButton = panel.querySelector<HTMLButtonElement>("[data-root]")!;
  const rootStatus = panel.querySelector<HTMLElement>("[data-root-status]")!;
  const observeRoot = async (request: Promise<CmdResult>) => {
    rootButton.disabled = true;
    rootStatus.textContent = `${vmId}：等待Shell Root结果；可继续刷新画面并手动选择Magisk授权。`;
    try {
      const result = await request;
      if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
      if (panel.isConnected) rootStatus.textContent = `${vmId}：本次Shell id已返回UID0；请另做冷启动持久性与版本验证。`;
    } catch (error) {
      if (panel.isConnected) rootStatus.textContent = `${vmId}：Root请求未通过：${error}；请刷新核对管理器状态后手动重试。`;
    } finally { rootButton.disabled = rootRequests.has(vmId); }
  };
  rootButton.addEventListener("click", (event: MouseEvent) => {
    if (!event.isTrusted || !panel.isConnected || rootRequests.has(vmId)) return;
    if (busy) { rootStatus.textContent = `${vmId}：请等待本次截图或点击结束，再请求Shell Root。`; return; }
    clearFrame();
    try { void observeRoot(requestRootOnce(vmId)); }
    catch (error) { rootStatus.textContent = `${vmId}：Root请求启动失败：${error}`; }
  });
  const pendingRoot = rootRequests.get(vmId);
  if (pendingRoot) void observeRoot(pendingRoot);
  status.textContent = `${vmId}：尚未采集；刷新前请确认 Android 已启动。点击默认关闭。`;
  image.addEventListener("error", () => {
    if (!panel.isConnected) return;
    clearFrame(); status.textContent = `${vmId}：内部 PNG 解码失败；请刷新画面。`;
  });
  input.addEventListener("change", () => {
    if (!frame || busy || performance.now() - frameStartedAt > FRAME_INPUT_MAX_AGE_MS) {
      disarm(); status.textContent = `${vmId}：画面已过期或尚未就绪，请刷新画面后重试。`; return;
    }
    image.style.cursor = input.checked ? "crosshair" : "default";
  });
  button.addEventListener("click", async () => {
    if (busy || !panel.isConnected) return;
    busy = true; button.disabled = true; clearFrame();
    frameStartedAt = performance.now();
    status.textContent = `${vmId}：正在采集内部画面…`;
    try {
      const captured = await api.captureVmGuest(vmId);
      if (!panel.isConnected) return;
      if (captured.vm_id !== vmId || captured.source !== "guest" || !captured.png_base64.startsWith("iVBORw0KGgo")
          || !Number.isInteger(captured.width) || !Number.isInteger(captured.height)
          || captured.width < 1 || captured.height < 1 || captured.width > 32768 || captured.height > 32768
          || !BOOT_ID.test(captured.boot_id) || !/^[a-f0-9]{64}$/i.test(captured.sha256)) throw new Error("截图身份或格式校验失败");
      image.src = `data:image/png;base64,${captured.png_base64}`;
      await image.decode();
      if (!panel.isConnected) return;
      if (image.naturalWidth !== captured.width || image.naturalHeight !== captured.height) throw new Error("PNG尺寸与响应元数据不一致");
      frame = captured; image.hidden = false; image.style.display = "block";
      input.disabled = performance.now() - frameStartedAt > FRAME_INPUT_MAX_AGE_MS;
      status.textContent = `${vmId} · ${captured.serial} · ${captured.width}×${captured.height} · ${captured.captured_at}（静态截图）`;
      evidence.textContent = `来源：guest screencap；boot：${captured.boot_id}；SHA256：${captured.sha256}`;
    } catch (error) {
      clearFrame(); if (panel.isConnected) status.textContent = `${vmId}：内部截图失败：${error}`;
    } finally { busy = false; button.disabled = false; }
  });
  image.addEventListener("click", async (event: MouseEvent) => {
    if (!event.isTrusted || !panel.isConnected || busy || !input.checked || !frame || image.hidden) return;
    const captured = frame;
    if (performance.now() - frameStartedAt > FRAME_INPUT_MAX_AGE_MS) {
      disarm(); status.textContent = `${vmId}：画面已超过30秒，请刷新画面后重试。`; return;
    }
    const point = mapGuestPoint(event.clientX, event.clientY, image.getBoundingClientRect(), captured.width, captured.height);
    if (!point) { status.textContent = `${vmId}：仅图片内容区域支持点击。`; return; }
    event.preventDefault(); busy = true; button.disabled = true; disarm(); frame = null;
    status.textContent = `${vmId}：发送一次guest点击 (${point.x}, ${point.y})…`;
    try {
      const result = await api.tapVmGuest(vmId, point.x, point.y, captured.width, captured.height, captured.boot_id);
      if (!result.ok) throw new Error(result.stderr || result.stdout || `exit=${result.code}`);
      if (panel.isConnected) status.textContent = `${vmId}：已发送一次guest点击 (${point.x}, ${point.y})；请刷新核对结果，当前仍是点击前截图。`;
    } catch (error) {
      if (panel.isConnected) status.textContent = `${vmId}：guest点击失败：${error}；请刷新画面后重试。`;
    } finally { busy = false; button.disabled = false; }
  });
  return panel;
}