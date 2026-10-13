import { createRollingClock } from "./rolling-clock";
import { animationTime, openingTime, openingDuration, navigationDuration } from "./animation-speed";
import { pageFor, resolveTab, stateLabel } from "./admin-pages";
import { capturePanelState, restorePanelState } from "./form-state";
import { loadArchivePage } from "./rinbot-runtime";
import { apiFetch, authenticateSession, getAdminToken } from "./api-client";
import { behaviorFormMarkup, readBehaviorForm, readTimeForm, timeFormMarkup, timeWindowMarkup } from "./policy-forms";
import "./rinbot-bridge.css";
import { cancelQueuedTask, cancelProactiveWake, clearMemoryHandoff, createStickerEntry, deleteStickerEntry, decideIncomingFriendRequest, loadDailyMomentArchive, loadMemoryConsolidationHistory, loadModelMemoryMutationHistory, loadMemoryHandoffs, loadStickerArchive, loadStickerPreview, restoreMemoryConsolidationRevision, restoreNodeMemoryConsolidationBackup, restoreModelMemoryRemoval, saveMemoryHandoff, syncStickerFavorites, updateStickerEntry, loadProactiveData, loadQzoneInteractionsAudit, runQzoneInteractions, loadRelationshipDetail, loadRelationshipEvaluationTrace, mergeIdentityPerson, reconcileUnknownOutbox, refreshRuntime, retryFriendRequestNotification, retryPrivateWhitelist, resolveDailyMomentPublication, publishDailyMomentDraft, runDailyMomentPreview, runMemoryConsolidation, runtime, saveAsrSettings, saveBehaviorSettings, saveChatControl, saveDailyMomentsSettings, saveModelSettings, saveProactiveSettings, saveQueueSettings, saveTimeControlSettings, saveWebSearchSettings, saveRelationshipPilotSettings, saveQzoneInteractionsSettings, saveGroupInviteSettings, saveStickerLifecycle, saveStickerCollectionLifecycle, startRuntime, subscribeRuntime, updateIdentityProfile, updateIncident, updateIncidentLifecycle, updateIncidentOperationalSettings, updateProactiveLifecycle, updateMemoryConsolidationLifecycle, updateMultimodalContextLifecycle, type AsrSettings, type BehaviorSettings, type DailyMomentsSettings, type ModelSettings, type ProactiveSettings, type QueueSettings, type TimeControlSettings, type WebSearchSettings, type RelationshipPilotSettings, type QzoneInteractionsSettings, type GroupInviteSettings } from "./rinbot-runtime";
import { InspectionOverlay } from "./inspection-overlay";
import { DocumentDecryption } from "./document-decryption";
import "./document-decryption.css";
import "./decryption.css";
import { escapeHtml } from "./html";
import { normalizeQuality, qualityPresets, type QualityPreset, type RenderQuality } from "./render-quality";
import { qualityMarkup, syncQualityUI } from "./quality-settings";
import { superPerformanceQuality, wallpaperQuality } from "./wallpaper-quality";
import "@kitlangton/rolling-number/styles.css";
import "./style.css";
import "./quality-settings.css";
import "./responsive.css";
import "./rinbot-runtime.css";
import { viewportLayout, openingLayout } from "./viewport-layout";
import { assetUrl } from "./asset-url";
import { initPwa, pwaSettingsMarkup } from "./pwa";
import { createRollingNumber, createRollingText } from "@kitlangton/rolling-number";
import { ArchiveScene } from "./scene";
import { ContentTransition, SurfaceTransition } from "./ui-transitions";
import { BootSequence } from "./boot";
import { loadBootWebfonts } from "./boot-lettering";
import { wrap, type ArchiveNavigation } from "./archive-loop";
import {
  records,
  categories,
  archiveColumns,
  columnFiles,
  fileLocation,
  managementNavigation,
  archiveSelection,
  restoreSavedArchiveKeys,
} from "./data";
import { TerminalAudio } from "./audio";
import { audioSettingsMarkup } from "./audio-settings";
import {
  createMotionPreferences,
  fullMotion,
  motionEnabled,
  motionPresetFor,
  motionSettingsMarkup,
  motionSummary,
  reducedMotion,
  type MotionKey,
  type MotionPreset,
  type StoredMotion,
} from "./motion-preferences";
import { StartupGate } from "./startup";
import { isWallpaper, wallpaperHost, wallpaperFrame, type WallpaperProperties } from "./wallpaper";
import "./startup.css";
import "./wallpaper.css";
import { Workbench } from "./workbench";
let workbench: Workbench | undefined;
import { ArchivePlayground } from "./archive-playground";
import { ARRAY_OPENING_END, openingShowsDetail } from "./wallpaper-opening";
import { paintTheme, themeSettingsMarkup } from "./theme-ui";
let playground: ArchivePlayground | undefined;
import { WallpaperEffects } from "./wallpaper-effects";
import { WallpaperBackground } from "./wallpaper-background";
let wallpaperEffects: WallpaperEffects | undefined;

const $ = <T extends HTMLElement = HTMLElement>(selector: string) =>
  document.querySelector<T>(selector)!;
import { logo, brandHeading } from "./brand";

$("#stage").innerHTML = `
  <div id="three-scene" class="three-scene"></div>
  <div class="scene-atmosphere archive-atmosphere"></div>
  <div id="boot-background" class="boot-background"><svg viewBox="0 0 1920 1080" preserveAspectRatio="none"><g fill="none" stroke="#fff" stroke-width="3"><path d="M-210 705C-45 705 182 704 247 567C337 377 99 306 4 435S27 680 169 631C309 584 227 314 279 111S568-113 568-113"/><path d="M1560-80C1374 114 1671 168 1601 323S1371 367 1431 480S1692 666 1559 787S1329 886 1498 1130"/><circle cx="1450" cy="648" r="346"/><circle cx="1450" cy="648" r="348"/></g></svg></div>
  <header class="brand">${brandHeading}</header>
  <nav class="system-nav" aria-label="系统导航">
    <button data-action="search"><span class="nav-glyph">⌕</span> 档案检索 <span class="key">/</span></button>
    <button data-action="saved" aria-label="查看收藏档案" title="收藏档案">＋ 收藏 <span id="saved-count">00</span></button>
    <button class="settings-button" data-action="settings" aria-label="系统设置" title="系统设置"><span class="settings-glyph" aria-hidden="true">◷</span><span class="settings-label">设置</span></button>
  </nav>
  <nav id="management-navigation" class="management-navigation" aria-label="管理页面导航"></nav>
  <button id="skip" class="skip" data-action="skip">进入系统 <span>↗</span></button>
  <section id="boot" class="boot" aria-label="系统启动">
    <div class="access-text">ACCESS</div>
    <div class="boot-logo">${logo}</div>
    <div class="auth-status"><span>▪</span> <span id="auth-message"></span><i></i></div>
    <div class="scan"><svg viewBox="0 0 1920 1080" aria-hidden="true"><g fill="none" stroke="#080a08" stroke-width="2" stroke-linecap="round"><path/><path stroke="#fff"/><path/><path/><path/><path/><circle class="orbit-dot" r="8" fill="#ed821b" stroke="none"/><circle class="orbit-dot" r="8" fill="#ed821b" stroke="none"/><circle class="scan-core" cx="960" cy="540" r="5" fill="#080a08" stroke="none"/></g></svg><span>权限校验通过</span></div>
    <div class="welcome"><div class="welcome-panel"></div><div class="welcome-heading">欢迎使用</div><div class="welcome-company"><strong>RInBot</strong><strong class="welcome-highlight" aria-hidden="true">RInBot</strong></div><div class="welcome-database">QQ 机器人管理终端</div><div class="welcome-logo">${logo}</div></div>
  </section>
  <svg id="inspection-marks" viewBox="0 0 1920 1080" aria-hidden="true"><path id="inspection-lines"/><g id="inspection-corners"></g><circle id="inspection-point" r="1.8"/></svg>
  <div id="inspection-text" aria-hidden="true">档案权限：<strong>本地系统使用</strong></div>
  <section id="archive-ui" class="archive-ui" aria-label="档案选择">
    <div class="archive-callout"><div class="eyebrow">RInBot 系统档案 <span>／</span> <span id="archive-category">功能分组</span></div><button class="file-title" data-action="open">档案编号：<span id="selected-id">X-<span id="selected-code">001</span></span><span class="file-open">↗</span></button><div class="callout-rule"><i></i></div><div class="file-summary"><span id="selected-title">消息档案</span><span id="selected-clearance">状态</span></div><button class="read-file" data-action="open">查看详情 <span>→</span></button></div>
    <div id="hover-label" class="hover-label" hidden>X-<span id="hover-code">001</span> / <span id="hover-title"></span></div>
    <div class="archive-counter"><span class="tiny-label">档案 / 当前编号</span><div><span id="selected-number">01</span><i>/</i><span class="count-total">${records.length}</span></div></div>
    <div class="archive-navigation"><button data-action="prev" aria-label="上一个档案">↑</button><div id="file-ticks" class="file-ticks"></div><button data-action="next" aria-label="下一个档案">↓</button></div>
    <div class="column-navigation"><button data-action="column-prev" aria-label="上一列">←</button><div><span id="column-number">分组 <span id="column-index">03</span> / 05</span><strong id="column-name">自动化档案</strong></div><button data-action="column-next" aria-label="下一列">→</button></div>
    <div class="archive-hint"><kbd>←</kbd> <kbd>→</kbd> 切换列 <span>／</span> <kbd>↑</kbd> <kbd>↓</kbd> 前后档案 <span>／</span> <kbd>ENTER</kbd> 读取</div>
  </section>
  <section id="detail-ui" class="detail-ui" aria-label="档案内容" hidden>
    <button class="back-button" data-action="back">← <span>返回档案总览</span><small>ESC</small></button>
    <div class="object-caption"><span id="object-id">档案 X-001</span><div>RInBot 系统档案</div></div>
    <article id="detail-content" class="detail-content"></article>
  </section>
  <div class="powered">视觉设计沿用 <b>RHINE LAB</b><i></i></div>
  <footer class="system-footer"><span><i class="status-light"></i> 本地终端已就绪${isWallpaper ? '<button type="button" class="three-toggle" data-action="toggle-three" aria-pressed="true" title="卸载三维模型，保留二维界面">3D 开启</button>' : ''}</span><span>RInBot <i>／</i> <span id="clock">00:00:00</span></span><button data-action="replay" title="重播启动流程">重新播放 ↗</button></footer>
  <div id="pwa-update-notice" class="pwa-update-notice" role="status" hidden><span>新版本已就绪</span><button data-pwa-action="update">更新并重启 ↻</button></div>
  <div id="modal-root"></div><div id="toast" class="toast" role="status"></div>
  <div id="loading" class="loading"><div class="loading-mark">${logo}</div><span>正在连接 RInBot 本地服务</span><i></i></div>
`;

$("#boot-background").insertAdjacentHTML(
  "beforeend",
  '<div class="boot-white"></div>',
);
const bootSequence = new BootSequence($("#stage"));
$("#viewport").insertAdjacentHTML("beforeend", '<button class="mobile-entry" data-action="skip">进入档案 <span>→</span></button>');

type Mode = "boot" | "archive" | "detail";
let mode: Mode = "boot",
  selected = 0,
  bootStart = 0,
  lastStep = "",
  ready = false;
let modal: "search" | "saved" | "settings" | null = null,
  searchQuery = "",
  filter = "全部档案";
let activeTab = "";
const reviewParams = new URLSearchParams(location.search);
let frozenTime =
  reviewParams.get("freeze") === "1"
    ? Number(reviewParams.get("time") ?? 0)
    : null;
if (reviewParams.get("review") === "1") {
  $("#stage").dataset.review = "true";
  window.addEventListener("message", (event) => {
    if (
      event.origin !== location.origin ||
      event.source !== window.parent ||
      event.data?.type !== "rhine-review-frame"
    )
      return;
    const t = Number(event.data.time);
    if (!Number.isFinite(t) || t < 0 || t >= 35) return;
    frozenTime = t;
    if (ready && mode !== "boot") setMode("boot");
  });
}
let toastTimer: ReturnType<typeof setTimeout>;
let previousFocus: HTMLElement | null = null;
const detailTransition = new SurfaceTransition($("#detail-ui"), undefined, navigationDuration(180), navigationDuration(180));
const tabTransition = new ContentTransition(navigationDuration(150));
$("#stage").style.setProperty("--navigation-tab-duration", `${navigationDuration(180)}ms`);
let modalTransition: SurfaceTransition | undefined;
let modalClosing = false;
let modalSiblings: { node: HTMLElement; inert: boolean }[] = [];
let pendingDetailFocus = false;
let bookmarkFeedback: Animation | undefined;
function readLocal<T>(key: string, fallback: T): T {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null") ?? fallback;
  } catch {
    return fallback;
  }
}
const saved = restoreSavedArchiveKeys(readLocal<unknown>("rhine-saved", []));
const storedPrefs = readLocal<Partial<{ sound: boolean; music: boolean; soundVolume: number; musicVolume: number; reduced: boolean; quality: boolean; rendering: RenderQuality; superPerformance: boolean; colorTheme: "light" | "dark"; motion: StoredMotion; motionPreset: MotionPreset }>>("rhine-settings", {});
const initialMotion = createMotionPreferences(
  storedPrefs.motion,
  storedPrefs.reduced ?? (storedPrefs.motion === undefined
    ? matchMedia("(prefers-reduced-motion: reduce)").matches
    : undefined),
);
const initialMotionPreset = motionPresetFor(initialMotion);
const prefs = {
  sound: storedPrefs.sound ?? true,
  music: storedPrefs.music ?? storedPrefs.sound ?? true,
  soundVolume: storedPrefs.soundVolume ?? .55,
  musicVolume: storedPrefs.musicVolume ?? .5,
  motion: initialMotion,
  motionPreset: initialMotionPreset,
  quality: storedPrefs.quality ?? true,
  superPerformance: storedPrefs.superPerformance ?? false,
  rendering: normalizeQuality(storedPrefs.rendering, storedPrefs.quality !== false),
  colorTheme: storedPrefs.colorTheme === "dark" ? "dark" : "light",
};
const motionActive = (key: MotionKey) => motionEnabled(prefs.motion, key);
const motionIsReduced = () => Object.values(prefs.motion).every((value) => !value);
paintTheme(prefs.colorTheme === "dark" ? 1 : 0);
const rollingMotion = {
  duration: 460,
  motionBlur: true,
  animated: motionActive("rollingNumbers"),
};
const updateFooterClock = createRollingClock($("#clock"));
const numberOptions = {
  ...rollingMotion,
  locales: "en-US",
  format: { minimumIntegerDigits: 2, useGrouping: false },
};
const fileCounter = createRollingNumber($("#selected-number"), {
  ...numberOptions,
  value: 1,
});
const columnCounter = createRollingNumber($("#column-index"), {
  ...numberOptions,
  value: 3,
});
const codeOptions = {
  ...numberOptions,
  format: { minimumIntegerDigits: 3, useGrouping: false },
  value: 1,
};
const textOptions = {
  ...rollingMotion,
  animated: motionActive("rollingText"),
  transition: "direct" as const,
  stagger: "none" as const,
};
const selectionTitle = createRollingText($("#selected-title"), {
  ...textOptions,
  text: $("#selected-title").textContent ?? "",
});
const columnTitle = createRollingText($("#column-name"), {
  ...textOptions,
  text: $("#column-name").textContent ?? "",
});
const hoverTitle = createRollingText($("#hover-title"), { ...textOptions, text: "" });
const categoryTitle = createRollingText($("#archive-category"), {
  ...textOptions,
  text: $("#archive-category").textContent ?? "",
});
const clearanceTitle = createRollingText($("#selected-clearance"), {
  ...textOptions,
  text: $("#selected-clearance").textContent ?? "",
});
const rollingTitles = [selectionTitle, columnTitle, hoverTitle, categoryTitle, clearanceTitle];
const selectedCode = createRollingNumber($("#selected-code"), codeOptions);
const hoverCode = createRollingNumber($("#hover-code"), codeOptions);
const audio = new TerminalAudio();
let musicSuppressed = false;
function configureAudio() { audio.configure({ ...prefs, music: prefs.music && !musicSuppressed }); }
configureAudio();
const reviewEntry = reviewParams.has("scene") || reviewParams.has("time") || reviewParams.get("review") === "1";
let started = false;
const loading = $("#loading");
// The entry screen uses the actual viewport, including portrait phones; the
// reference animation still uses its calibrated 1920 x 1080 stage.
$("#viewport").append(loading);
$("#stage").inert = true;
$(".mobile-entry").inert = true;
const entry = !isWallpaper && !reviewEntry && (prefs.sound || prefs.music) ? new StartupGate({
  root: loading,
  unlock: () => audio.unlock(),
  cancel: () => audio.cancelEntry(),
  start: silent => completeStartup(silent),
}) : undefined;
if (entry) {
  audio.holdForEntry();
  if (prefs.music) void audio.prepareMusic().catch(() => { /* Entry offers retry. */ });
}
let audioPreview = false, audioPreviewRequest = 0;
let scene: ArchiveScene | undefined;
let threeState: "on" | "closing" | "off" | "loading" = "on";
let resumeCell: { lane: number; row: number } | undefined;
let resumeSelection = -1;
const accessLog: { id: string; time: string }[] = [];
const columnMemory = archiveColumns.map((_, lane) => columnFiles(lane)[0]);
function recordAccess() {
  accessLog.unshift({
    id: records[selected].id,
    time: new Date().toLocaleTimeString("en-GB"),
  });
}
function saveAudioPrefs() {
  try {
    localStorage.setItem("rhine-settings", JSON.stringify(prefs));
  } catch {}
  configureAudio();
}
function superPerformanceEnabled() { return isWallpaper ? wallpaperHost()?.properties.superperformance?.value === true : prefs.superPerformance; }
function effectiveRenderQuality() { return superPerformanceEnabled() ? superPerformanceQuality : prefs.rendering; }
function savePrefs() {
  saveAudioPrefs();
  if (!motionActive("rollingText")) rollingTitles.forEach(title => title.finish());
  if (!motionActive("rollingNumbers")) [fileCounter, columnCounter, selectedCode, hoverCode].forEach(counter => counter.finish());
  if (!motionActive("surfaceTransitions")) {
    detailTransition.finish();
    modalTransition?.finish();
    tabTransition.finish();
    bookmarkFeedback?.cancel();
  }
  scene?.setMotion(prefs.motion);
  scene?.setTheme(prefs.colorTheme === "dark", !motionActive("surfaceTransitions") || !started);
  document.querySelectorAll<HTMLElement>("[data-color-theme]").forEach(button => button.setAttribute("aria-pressed", String(button.dataset.colorTheme === prefs.colorTheme)));
  scene?.setSuperPerformance(superPerformanceEnabled());
  scene?.setQuality(effectiveRenderQuality());
  syncQualityUI(prefs.rendering);
  updateQualitySummary();
  fileCounter.update({ animated: motionActive("rollingNumbers") && mode === "archive" });
  rollingTitles.forEach(title => title.update({ animated: motionActive("rollingText") && mode === "archive" }));
  columnCounter.update({ animated: motionActive("rollingNumbers") && mode === "archive" });
  selectedCode.update({ animated: motionActive("rollingNumbers") && mode === "archive" });
  hoverCode.update({ animated: motionActive("rollingNumbers") && mode === "archive" });
  $("#stage").classList.toggle("reduce-motion", motionIsReduced());
  $("#stage").classList.toggle("reduce-surfaces", !motionActive("surfaceTransitions"));
  workbench?.setMotion(prefs.motion);
  updateFooterClock(new Date(), motionActive("rollingNumbers"));
  syncWallpaperBackground();
}
let previousLayout = "";
function fit() {
  const stage = $("#stage");
  const viewport = $("#viewport");
  const coarse = matchMedia("(pointer: coarse)").matches;
  const reference = reviewParams.has("time") || reviewParams.get("review") === "1";
  const { width, height, scale, kind } = mode === "boot" && !reference
    ? openingLayout(viewport.clientWidth, viewport.clientHeight)
    : viewportLayout(viewport.clientWidth, viewport.clientHeight, coarse, mode === "boot");
  stage.style.width = `${width}px`;
  stage.style.height = `${height}px`;
  stage.style.transform = `translate(-50%, -50%) scale(${scale})`;
  stage.dataset.layout = kind;
  stage.dataset.touch = String(coarse);
  viewport.dataset.mobileBoot = String(mode === "boot" && (coarse || viewport.clientWidth < 1100));
  stage.style.setProperty("--stage-scale", String(scale));
  stage.style.setProperty("--opening-width", `${width}px`);
  stage.style.setProperty("--opening-height", `${height}px`);
  stage.style.setProperty("--opening-scan-scale", String(Math.min(1, width / 1920)));
  stage.dataset.openingPortrait = String(width < height);
  // The software keyboard resizes dialogs without recomposing the 3D scene.
  const visible = window.visualViewport;
  const stageTop = (viewport.clientHeight - height * scale) / 2;
  stage.style.setProperty("--modal-top", `${Math.max(0, (visible?.offsetTop ?? 0) - stageTop) / scale}px`);
  stage.style.setProperty("--modal-height", `${Math.min(height, (visible?.height ?? viewport.clientHeight) / scale)}px`);
  $("#viewport").style.setProperty("--scale", String(scale));
  const marks = document.querySelector("#inspection-marks");
  marks?.setAttribute("viewBox", `0 0 ${width} ${height}`);
  const layoutKey = JSON.stringify([width, height, scale, kind, devicePixelRatio]);
  if (layoutKey !== previousLayout) {
    previousLayout = layoutKey;
    scene?.resize();
  }
  updateQualitySummary();
  // Re-measure line covers and tab underline after wrapping changes.
  requestAnimationFrame(() => {
    documentDecryption.refresh();
    const tab = document.querySelector<HTMLElement>(".detail-tabs button.active");
    const indicator = document.querySelector<HTMLElement>(".tab-indicator");
    if (tab && indicator) indicator.style.transform = `translateX(${tab.offsetLeft}px) scaleX(${tab.offsetWidth})`;
  });
}
window.addEventListener("resize", fit);
window.visualViewport?.addEventListener("resize", fit);
window.visualViewport?.addEventListener("scroll", fit);
matchMedia("(pointer: coarse)").addEventListener("change", fit);
fit();
$("#file-ticks").innerHTML = columnFiles(fileLocation(selected).lane)
  .map(
    (index) => `<button data-select="${index}"></button>`,
  )
  .join("");
let fileTicks = [...$("#file-ticks").querySelectorAll<HTMLButtonElement>("button")];
let fileTicksLane = fileLocation(selected).lane;

function setMode(next: Mode) {
  if (workbench?.enabled && next === "detail") next = "archive";
  const previousMode = mode;
  rollingTitles.forEach(title => title.update({ animated: motionActive("rollingText") && next === "archive" }));
  if (next !== "archive") {
    rollingTitles.forEach(title => title.finish());
    hoverCode.finish();
    $("#hover-label").hidden = true;
  }
  if (next === "detail" && mode !== "detail") recordAccess();
  mode = next;
  syncWallpaperBackground();
  audio.setScene(next);
  if (next !== "boot" && audioPreview) {
    audioPreview = false;
    audioPreviewRequest++;
    configureAudio();
  }
  $("#stage").dataset.mode = next;
  workbench?.syncVisibility();
  if (previousMode !== next) fit();
  $("#boot").inert = next !== "boot";
  $("#boot").setAttribute("aria-hidden", String(next !== "boot"));
  $("#archive-ui").inert = next !== "archive" || Boolean(modal) || Boolean(workbench?.enabled);
  $("#archive-ui").setAttribute("aria-hidden", String(next !== "archive" || Boolean(workbench?.enabled)));
  $(".system-nav").inert = next === "boot" || Boolean(modal);
  $("#management-navigation").inert = next === "boot" || Boolean(modal);
  $(".system-footer").inert = next === "boot" || Boolean(modal);
  if (next === "detail") {
    if (previousMode !== "detail") detailTransition.show(!motionActive("surfaceTransitions"));
  } else if (previousMode === "detail" || (next === "boot" && !$("#detail-ui").hidden)) {
    pendingDetailFocus = false;
    tabTransition.cancel();
    detailTransition.hide(!motionActive("surfaceTransitions") || next === "boot");
    if (!modal && next === "archive") $(".read-file").focus({ preventScroll: true });
  }
  $("#detail-ui").inert = next !== "detail" || Boolean(modal);
  scene?.setMode(next === "boot" ? "hidden" : next);
  if (next !== "boot") {
    bootSequence.reset();
    $(".file-title").firstChild!.textContent = "档案编号：";
    $("#stage").dataset.boot = "done";
  }
  if (next === "detail" && previousMode !== "detail") {
    renderDetail();
    pendingDetailFocus = true;
    if (!scene) {
      $("#detail-content").style.opacity = "1";
      $("#detail-content").style.translate = "0 0";
      $("#detail-content").inert = false;
    }
  }
}
function select(index: number, navigation?: ArchiveNavigation) {
  selected = (index + records.length) % records.length;
  columnMemory[fileLocation(selected).lane] = selected;
  if (mode === "detail") setMode("archive");
  activeTab = resolveTab(records[selected].title, new URLSearchParams(location.search).get("tab")).id;
  scene?.select(selected, navigation);
  updateSelection(navigation);
  const columnMove = navigation && "axis" in navigation && navigation.axis === "lane";
  audio.play(columnMove ? "column" : "tick", columnMove ? navigation.direction * .45 : 0);
}
function stepFile(direction: number) {
  const files = columnFiles(fileLocation(selected).lane);
  if (files.length < 2) return;
  select(
    files[(files.indexOf(selected) + direction + files.length) % files.length],
    { axis: "row", direction },
  );
}
function stepColumn(direction: number) {
  const lane = fileLocation(selected).lane;
  const next = wrap(lane + direction, archiveColumns.length);
  select(columnMemory[next], { axis: "lane", direction });
}
function updateSelection(navigation?: ArchiveNavigation) {
  const selection = archiveSelection(selected);
  const r = selection.record;
  const { lane } = fileLocation(selected);
  const files = columnFiles(lane);
  if (fileTicksLane !== lane || fileTicks.length !== files.length) {
    $("#file-ticks").innerHTML = files.map(() => "<button></button>").join("");
    fileTicks = [...$("#file-ticks").querySelectorAll<HTMLButtonElement>("button")];
    fileTicksLane = lane;
  }
  selectionTitle.update({ text: r.title, animated: motionActive("rollingText") && mode === "archive" });
  clearanceTitle.update({ text: r.status, animated: motionActive("rollingText") && mode === "archive" });
  categoryTitle.update({ text: r.category, animated: motionActive("rollingText") && mode === "archive" });
  const direction =
    navigation && "axis" in navigation
      ? navigation.direction > 0
        ? "up"
        : "down"
      : "auto";
  selectedCode.update({
    value: Number(r.id.slice(2)),
    animated: motionActive("rollingNumbers") && mode === "archive",
    direction,
  });
  fileCounter.update({
    value: selection.number,
    animated: motionActive("rollingNumbers") && mode === "archive",
    direction:
      navigation && "axis" in navigation && navigation.axis === "row"
        ? direction
        : "auto",
  });
  $(".count-total").textContent = String(selection.total).padStart(2, "0");
  columnCounter.update({
    value: lane + 1,
    animated: motionActive("rollingNumbers") && mode === "archive",
    direction:
      navigation && "axis" in navigation && navigation.axis === "lane"
        ? direction
        : "auto",
  });
  columnTitle.update({ text: archiveColumns[lane], animated: motionActive("rollingText") && mode === "archive" });
  $<HTMLButtonElement>('[data-action="column-prev"]').disabled = false;
  $<HTMLButtonElement>('[data-action="column-next"]').disabled = false;
  fileTicks.forEach((button, slot) => {
    const index = files[slot], record = records[index];
    button.dataset.select = String(index);
    button.setAttribute("aria-label", `选择档案 ${record.id} ${record.title}`);
    button.title = `${record.id} · ${record.title}`;
    button.classList.toggle("selected", index === selected);
    button.setAttribute("aria-pressed", String(index === selected));
  });
  $("#saved-count").textContent = String(saved.size).padStart(2, "0");
  syncManagementNavigation();
}
function currentManagementItem() {
  const rawPath = location.pathname.replace(/\/$/, "");
  const path = rawPath === "/overview" ? "/overview/overview" : rawPath;
  return managementNavigation.flatMap((group) => group.items.map((item) => ({ group, item })))
    .find(({ group, item }) => `/${group.id}/${item.id}` === path)?.item;
}
const recentManagementItem = new Map<string, string>(
  managementNavigation.map((group) => [group.id, group.items[0].id]),
);
function syncManagementNavigation() {
  const root = $("#management-navigation");
  const active = currentManagementItem() ?? managementNavigation.flatMap((entry) => entry.items.map((item) => ({ id: item.id, label: item.label, title: item.title }))).find((item) => pageFor(item.title).tabs.some((tab) => tab.view === resolveTab(records[selected].title, activeTab).view)) ?? managementNavigation[0].items[0];
  const group = managementNavigation.find((entry) => entry.items.some((item) => item.id === active.id))!;
  recentManagementItem.set(group.id, active.id);
  root.innerHTML = `<div class="management-primary" role="navigation" aria-label="一级菜单">${managementNavigation.map((entry) => `<button type="button" data-nav-group="${entry.id}" aria-current="${entry.id === group.id ? "page" : "false"}">${entry.label}</button>`).join("")}</div><div class="management-secondary" role="navigation" aria-label="二级菜单">${group.items.map((item) => `<button type="button" data-nav-item="${item.id}" aria-current="${item.id === active.id ? "page" : "false"}">${item.label}</button>`).join("")}</div><div class="management-breadcrumb" aria-label="当前位置"><span>${escapeHtml(group.label)}</span><span aria-hidden="true">／</span><strong>${escapeHtml(active.label)}</strong>${mode === "detail" ? `<span aria-hidden="true">／</span><span>${escapeHtml(resolveTab(records[selected].title, activeTab).label)}</span>` : ""}</div>`;
}
function navigateManagement(itemId: string, addHistory = true) {
  const group = managementNavigation.find((entry) => entry.items.some((item) => item.id === itemId));
  const item = group?.items.find((entry) => entry.id === itemId);
  if (!group || !item) return;
  const path = `/${group.id}/${item.id}`;
  if (addHistory && location.pathname !== path) history.pushState({ managementRoute: path }, "", path);
  if (modal) {
    closeModal(() => navigateManagement(itemId, false));
    return;
  }
  const index = records.findIndex((record) => record.title === item.title);
  if (index < 0) {
    notify(`页面暂未接入：${item.label}`);
    return;
  }
  select(index);
  if (mode !== "detail") setMode("detail");
  syncManagementNavigation();
}
$("#management-navigation").addEventListener("click", (event) => {
  const target = (event.target as HTMLElement).closest<HTMLButtonElement>("button");
  if (!target) return;
  if (target.dataset.navItem) navigateManagement(target.dataset.navItem);
  else if (target.dataset.navGroup) {
    const group = managementNavigation.find((entry) => entry.id === target.dataset.navGroup);
    if (group) {
      const remembered = group.items.find((item) => item.id === recentManagementItem.get(group.id));
      navigateManagement((remembered ?? group.items[0]).id);
    }
  }
});
window.addEventListener("popstate", () => {
  messageQuery = new URLSearchParams(location.search).get("q") ?? "";
  const item = currentManagementItem();
  if (item) navigateManagement(item.id, false);
});
syncManagementNavigation();
function replayBoot(forcePreview = false) {
  if (!ready) return;
  closeModal(() => replayBootAfterModal(forcePreview));
}
function replayBootAfterModal(forcePreview: boolean) {
  bootStart = performance.now() / 1000 - 1.76;
  frozenTime = null;
  lastStep = "";
  setMode(!motionActive("boot") && !forcePreview ? "archive" : "boot");
  audio.restartBoot();
  scene?.select(0);
  selected = 0;
  updateSelection();
  if (!forcePreview) audio.play("ui-tick");
}
function openFile() {
  if (!ready) return;
  closeModal(() => {
    const view = resolveTab(records[selected].title, activeTab).view;
    const destination = managementNavigation.flatMap((group) => group.items.map((item) => ({ group, item }))).find(({ item }) => pageFor(item.title).tabs.some((tab) => tab.view === view));
    if (destination) {
      const tab = pageFor(destination.item.title).tabs.find((entry) => entry.view === view)!;
      history.pushState(null, "", `/${destination.group.id}/${destination.item.id}?tab=${tab.id}`);
      navigateManagement(destination.item.id, false);
    } else setMode("detail");
    audio.play("open");
  });
}
function toggleSaved() {
  const id = records[selected].key;
  if (saved.has(id)) saved.delete(id);
  else saved.add(id);
  try {
    localStorage.setItem("rhine-saved", JSON.stringify([...saved]));
  } catch {}
  $("#saved-count").textContent = String(saved.size).padStart(2, "0");
  const button = document.querySelector<HTMLButtonElement>('[data-action="bookmark"]');
  if (!button) return;
  const added = saved.has(id);
  button.firstChild!.textContent = added ? "− 取消收藏" : "＋ 收藏档案";
  button.querySelector("span")!.textContent = added ? "已收藏" : "收藏档案";
  button.setAttribute("aria-pressed", String(added));
  bookmarkFeedback?.cancel();
  if (motionActive("surfaceTransitions")) bookmarkFeedback = button.animate(
    [{ backgroundColor: "#67634c" }, { backgroundColor: "#252820" }],
    { duration: 220, easing: "ease-out" },
  );
  audio.play("confirm");
  notify(saved.has(id) ? "档案已加入收藏" : "已取消收藏");
}
function renderDetail() {
  tabTransition.cancel();
  const r = records[selected];
  const page = pageFor(r.title);
  $("#object-id").textContent = "档案 " + r.id;
  $("#detail-content").innerHTML = `
  <div class="detail-kicker"><span>RInBot 管理终端</span><span id="workspace-connection"></span></div>
  <div class="workspace-title"><h2>${escapeHtml(page.title)}</h2><button type="button" class="workspace-bookmark" data-action="bookmark" aria-label="收藏工作区" aria-pressed="${saved.has(r.key)}">${saved.has(r.key) ? "− 取消收藏" : "＋ 收藏"}<span hidden>${saved.has(r.key) ? "已收藏" : "收藏档案"}</span></button></div>
  <div class="detail-rule"></div>
  <div class="detail-tabs" role="tablist" aria-label="${escapeHtml(page.title)}功能页签">${page.tabs.map((tab) => `<button id="tab-${tab.id}" role="tab" aria-controls="tab-panel" aria-selected="false" data-tab="${tab.id}"><span>${tab.label}</span></button>`).join("")}<i class="tab-indicator" aria-hidden="true"></i></div>
  <div id="workspace-notice" role="status"></div>
  <div id="tab-panel" class="tab-panel" role="tabpanel"></div>`;
  $("#detail-content").setAttribute("tabindex", "-1");
  documentDecryption.reset($("#detail-content"), !motionActive("documentReveal") || !scene || scene.decryptionFrame.phase === "clear");
  setTab(activeTab, false);
}
function messageArchiveMarkup() {
  const query = messageQuery.toLocaleLowerCase();
  const messages = runtime.messages.filter((message) => (activeTab !== "group" || message.is_group) && (activeTab !== "private" || !message.is_group) && (!query || [message.text, message.sender_id, message.conversation_id].some((text) => text.toLocaleLowerCase().includes(query))));
  return `<div class="runtime-inline"><label class="runtime-row"><span>筛选本页（会话、QQ 号或正文）</span><input id="message-query" type="search" value="${escapeHtml(messageQuery)}" /></label><button type="button" class="solid-button" data-action="filter-messages">筛选</button>
    <div class="panel-label">本页显示 ${messages.length} / ${runtime.messages.length} 条 · 每页最多 ${runtime.pageSize} 条</div>
    ${messages.map((message) => `<article class="incident-card"><div class="incident-heading"><strong>${message.is_group ? "群聊" : "私聊"} ${escapeHtml(message.conversation_id)}</strong><span>${new Date(message.timestamp < 1e12 ? message.timestamp * 1000 : message.timestamp).toLocaleString("zh-CN")}</span></div><small>发送者 ${escapeHtml(message.sender_id)}</small><p class="message-text">${escapeHtml(message.text || "［媒体消息］")}</p></article>`).join("") || "<p>本页没有匹配的消息。</p>"}${paginationMarkup("messages", runtime.messageOffset, runtime.messages.length)}</div>`;
}
let messageQuery = new URLSearchParams(location.search).get("q") ?? "";
function paginationMarkup(kind: "messages" | "tasks", offset: number, count: number) {
  return `<div class="incident-actions archive-pagination"><button type="button" data-action="archive-page" data-kind="${kind}" data-offset="${Math.max(0, offset - runtime.pageSize)}" ${offset === 0 || runtime.pageLoading ? "disabled" : ""}>上一页</button><span>第 ${Math.floor(offset / runtime.pageSize) + 1} 页</span><button type="button" data-action="archive-page" data-kind="${kind}" data-offset="${offset + runtime.pageSize}" ${count < runtime.pageSize || runtime.pageLoading ? "disabled" : ""}>下一页</button></div>`;
}
function unknownOutboxMarkup() {
  const rows = runtime.unknown.map((item) => {
    let preview = item.payload;
    try {
      const payload = JSON.parse(item.payload) as { text?: unknown };
      if (typeof payload?.text === "string") preview = payload.text;
      else preview = JSON.stringify(payload) ?? item.payload;
    } catch {
      // Show the bounded raw payload for manual inspection if legacy JSON is malformed.
    }
    const updated = item.updated_at > 0
      ? new Date(item.updated_at).toLocaleString("zh-CN")
      : "时间未知";
    return `<article class="incident-card" data-unknown-outbox-card>
      <div class="incident-heading"><strong>${escapeHtml(item.conversation_id)}</strong><span>${escapeHtml(updated)}</span></div>
      <div class="runtime-row"><span>记录编号</span><b>${escapeHtml(item.id)}</b></div>
      <div class="runtime-row"><span>外发内容 · 最多显示 800 字</span><b>${escapeHtml(preview.slice(0, 800))}${preview.length > 800 ? "…" : ""}</b></div>
      <div class="incident-meta"><span>发送尝试 ${item.attempts}</span><span>${item.error ? escapeHtml(item.error.slice(0, 240)) : "无错误详情"}</span></div>
      <div class="incident-actions"><button type="button" class="solid-button" data-action="reconcile-unknown-outbox" data-outcome="sent" data-id="${escapeHtml(item.id)}">确认已送达</button><button type="button" class="solid-button" data-action="reconcile-unknown-outbox" data-outcome="failed" data-id="${escapeHtml(item.id)}">确认未送达</button></div>
    </article>`;
  }).join("");
  return `<div class="runtime-inline"><div class="panel-label">未知外发对账 · ${runtime.unknown.length} 项</div>
    <p>先在 QQ 中核对目标会话和实际消息，再选择结果。这里的对账只更新记录，不会重发消息；未知结果始终禁止自动重试。</p>
    ${rows || "<p>当前没有待核对的未知外发。</p>"}
    ${runtime.unknown.length ? `<label class="runtime-row"><span>管理员令牌（只用于本次对账）</span><input id="unknown-outbox-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><small id="unknown-outbox-status" role="status"></small>` : ""}
  </div>`;
}
function memoryHandoffMarkup() {
  const rows = runtime.memoryHandoffs.length
    ? runtime.memoryHandoffs.map(({ chatKey, handoff }) => {
      const listField = (name: string, label: string, values: string[]) => `<label class="runtime-row handoff-edit-field"><span>${label}（每行一条）</span><textarea data-handoff-field="${name}" rows="3">${escapeHtml(values.join("\n"))}</textarea></label>`;
      const ttlMinutes = Math.max(5, Math.round((handoff.expiresAt - handoff.updatedAt) / 60_000));
      return `<article class="incident-card" data-handoff-card data-chat-key="${escapeHtml(chatKey)}" data-updated-at="${handoff.updatedAt}">
        <div class="incident-heading"><strong>${escapeHtml(chatKey)}</strong><span>更新于 ${handoff.updatedAt ? new Date(handoff.updatedAt).toLocaleString("zh-CN") : "时间未知"}</span></div>
        ${handoff.topic ? `<div class="runtime-row"><span>当前话题</span><b>${escapeHtml(handoff.topic)}</b></div>` : ""}
        ${handoff.summary ? `<p>${escapeHtml(handoff.summary)}</p>` : ""}
        ${handoff.nextStep ? `<div class="runtime-row"><span>下一步</span><b>${escapeHtml(handoff.nextStep)}</b></div>` : ""}
        <div class="incident-meta"><span>参与者 ${handoff.participantIds.length} 人 · ${handoff.openQuestions.length} 项待确认</span><span>${handoff.expiresAt ? `有效至 ${new Date(handoff.expiresAt).toLocaleString("zh-CN")}` : "未设置过期时间"}</span></div>
        <details class="handoff-edit"><summary>编辑这条交接</summary><div class="runtime-inline">
          <label class="runtime-row"><span>当前话题</span><input data-handoff-field="topic" maxlength="200" value="${escapeHtml(handoff.topic)}" /></label>
          <label class="runtime-row"><span>有效时间（分钟）</span><input data-handoff-field="ttlMinutes" type="number" min="5" max="10080" value="${ttlMinutes}" /></label>
          <label class="runtime-row handoff-edit-field"><span>已知上下文</span><textarea data-handoff-field="summary" rows="3">${escapeHtml(handoff.summary)}</textarea></label>
          ${listField("hypotheses", "待验证假设", handoff.hypotheses)}${listField("evidence", "关键证据", handoff.evidence)}
          ${listField("facts", "已确认事实", handoff.facts)}${listField("decisions", "已作决定", handoff.decisions)}
          ${listField("rejectedDirections", "已排除方向", handoff.rejectedDirections)}${listField("openQuestions", "未解决问题", handoff.openQuestions)}
          <label class="runtime-row handoff-edit-field"><span>下一步意图</span><textarea data-handoff-field="nextStep" rows="2">${escapeHtml(handoff.nextStep)}</textarea></label>
          <button type="button" class="solid-button" data-action="memory-handoff-save">保存交接</button>
        </div></details>
        <button type="button" class="solid-button" data-action="memory-handoff-clear" data-chat-key="${escapeHtml(chatKey)}">清除此会话交接</button>
      </article>`;
    }).join("")
    : runtime.memoryHandoffsLoaded ? "<p>当前没有未过期的会话交接。</p>" : "<p>输入管理令牌后载入会话交接。</p>";
  return `<div class="runtime-inline"><div class="panel-label">会话交接 · 管理员专属</div>
    <small>交接用于同一会话的后续回复；停用保留记录。</small>
    <label class="runtime-row"><span>管理员令牌（只用于本次查看、编辑或清除）</span><input id="memory-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
    <button type="button" class="solid-button" data-action="memory-handoff-load">载入会话交接</button>
    ${runtime.memoryHandoffError ? `<p role="status">${escapeHtml(runtime.memoryHandoffError)}</p>` : ""}
    <div class="incident-list">${rows}</div></div>`;
}
function stickerArchiveMarkup() {
  const entries = runtime.stickers.length
    ? runtime.stickers.map((entry) => `<article class="incident-card sticker-card" data-sticker-id="${escapeHtml(entry.id)}">
        <div class="incident-heading"><strong>${escapeHtml(entry.note || entry.description || entry.id)}</strong><span>${entry.hidden ? "已隐藏" : entry.sendable ? "可用状态已记录" : "资源不可发送"}</span></div>
        <div class="runtime-row"><span>编号</span><b>${escapeHtml(entry.id)}</b></div>
        <label class="runtime-row"><span>说明</span><input data-sticker-field="desc" maxlength="240" value="${escapeHtml(entry.description)}" /></label>
        <label class="runtime-row"><span>本地备注</span><input data-sticker-field="localNote" maxlength="240" value="${escapeHtml(entry.note)}" /></label>
        <label class="runtime-row"><span>标签（每行一个）</span><textarea data-sticker-field="tags" rows="2">${escapeHtml(entry.tags.join("\n"))}</textarea></label>
        <label class="runtime-row"><span>用途</span><input data-sticker-field="usage" maxlength="240" value="${escapeHtml(entry.usage)}" /></label>
        ${entry.hasImage ? `<div class="sticker-preview-wrap">${runtime.stickerPreviewUrls[entry.id] ? `<img class="sticker-preview" src="${escapeHtml(runtime.stickerPreviewUrls[entry.id])}" alt="贴纸 ${escapeHtml(entry.id)}" />` : ""}<button type="button" class="solid-button" data-action="sticker-preview">${runtime.stickerPreviewUrls[entry.id] ? "重新载入图片" : "查看图片"}</button></div>` : "<p>此记录没有可读取的本地图片。</p>"}
        <div class="incident-meta"><span>来源：${escapeHtml(entry.source)} · 使用 ${entry.useCount} 次</span><span>${entry.sendErrorCode ? `资源状态：${escapeHtml(entry.sendErrorCode)} ${escapeHtml(entry.sendErrorMessage)}` : ""}</span></div>
        <div class="incident-actions"><button type="button" class="solid-button" data-action="sticker-save">保存资料</button><button type="button" class="solid-button" data-action="sticker-delete">${entry.source === "qq" ? "本地隐藏" : "删除本地记录"}</button></div>
      </article>`).join("")
    : runtime.stickerArchiveLoaded ? "<p>没有匹配的贴纸记录。</p>" : "<p>输入管理令牌后载入已迁移的贴纸档案。</p>";
  const syncReady = runtime.stickerLifecycle?.runtimeReady === true;
  const stickerSettings = runtime.stickerLifecycle?.settings;
  return `<div class="runtime-inline"><div class="panel-label">贴纸管理 · 受保护的操作页</div>
    <p>管理本地贴纸资料与图片，或在运行验收通过后同步 QQ 收藏。QQ 收藏的隐藏仅影响本地档案，不会修改 QQ；发送和收藏能力仍受独立运行门槛控制。</p>
    <div class="sticker-create"><div class="panel-label">模型使用偏好</div>
      <label class="runtime-row"><span>系统提示词中的贴纸数量（1–60）</span><input id="sticker-prompt-max" type="number" min="1" max="60" value="${stickerSettings?.promptMaxStickers ?? 10}" /></label>
      <label class="runtime-row"><span>使用倾向</span><select id="sticker-encourage"><option value="0" ${stickerSettings?.encourage === 0 ? "selected" : ""}>少用</option><option value="1" ${stickerSettings?.encourage === 1 || stickerSettings == null ? "selected" : ""}>适中</option><option value="2" ${stickerSettings?.encourage === 2 ? "selected" : ""}>较多</option><option value="3" ${stickerSettings?.encourage === 3 ? "selected" : ""}>喜欢用</option></select></label>
      <button type="button" class="solid-button" data-action="sticker-preferences-save">保存模型偏好</button>
    </div>
    <div class="sticker-create">
      <div class="panel-label">新增本地贴纸</div>
      <label class="runtime-row"><span>图片文件（PNG、JPEG、GIF、WebP，最大 8 MiB）</span><input id="sticker-image-file" type="file" accept="image/png,image/jpeg,image/gif,image/webp" /></label>
      <label class="runtime-row"><span>或图片网址（可选）</span><input id="sticker-image-url" type="url" maxlength="2048" placeholder="https://…" /></label>
      <label class="runtime-row"><span>说明</span><input id="sticker-create-desc" maxlength="240" /></label>
      <label class="runtime-row"><span>本地备注</span><input id="sticker-create-note" maxlength="240" /></label>
      <label class="runtime-row"><span>标签（每行一个）</span><textarea id="sticker-create-tags" rows="2"></textarea></label>
      <label class="runtime-row"><span>用途</span><input id="sticker-create-usage" maxlength="240" /></label>
      <button type="button" class="solid-button" data-action="sticker-create">新增贴纸</button>
    </div>
    <label class="runtime-row"><span>筛选词（编号、说明、备注或标签）</span><input id="sticker-search" type="search" maxlength="120" value="${escapeHtml(runtime.stickerQuery)}" /></label>
    <label class="runtime-row"><span>管理员令牌（只用于本次读取）</span><input id="sticker-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
    <button type="button" class="solid-button" data-action="sticker-archive-load">载入贴纸档案</button>
    <button type="button" class="solid-button" data-action="sticker-qq-sync" ${!syncReady || runtime.stickerSyncInProgress ? "disabled" : ""}>${runtime.stickerSyncInProgress ? "正在同步…" : "同步 QQ 收藏（最多 500 条）"}</button>
    ${runtime.stickerSyncStatus ? `<p role="status">${escapeHtml(runtime.stickerSyncStatus)}</p>` : ""}
    ${runtime.stickerArchiveError ? `<p role="status">${escapeHtml(runtime.stickerArchiveError)}</p>` : ""}
    ${runtime.stickerArchiveLoaded ? `<div class="incident-summary"><span>当前显示 <b>${runtime.stickers.length}</b> 项</span><span>单次最多 100 项</span></div>` : ""}
    <div class="incident-list">${entries}</div></div>`;
}
function dailyMomentsArchiveMarkup() {
  const statusLabels: Record<string, string> = {
    preview: "待审核草稿", skipped: "已跳过", published: "已发布",
    publishing: "发布中断待核对", "publish-unknown": "发布结果未知",
    "publish-missed": "确认未发布", running: "处理中", interrupted: "生成中断",
    failed: "失败", deferred: "已延后", missed: "错过计划",
  };
  const archivedSlotIds = new Set(runtime.dailyMomentRecords
    .map((record) => typeof record.slotId === "string" ? record.slotId : "")
    .filter(Boolean));
  const unlinkedGenerationDecisions = runtime.dailyMomentGenerationDecisions
    .filter((item) => !archivedSlotIds.has(item.slotId));
  const rows = runtime.dailyMomentRecords.length
    ? runtime.dailyMomentRecords.map((record) => `<article class="incident-card">
        <div class="incident-heading"><strong>${escapeHtml(record.dayKey ?? "日期未知")} · ${escapeHtml(record.source ?? "来源未知")}</strong><span>${escapeHtml(statusLabels[record.status ?? ""] ?? record.status ?? "状态未知")}</span></div>
        <div class="incident-meta"><span>${record.groupCount ?? 0} 个群 · 决策 ${escapeHtml(record.decision ?? "未记录")}</span><span>${record.publishAttempted ? `发布记录 ${escapeHtml(record.tid || "结果待核对")}` : "未尝试外部发布"}</span></div>
        ${record.reason ? `<p>${escapeHtml(record.reason)}</p>` : ""}
        ${record.error ? `<p role="status">${escapeHtml(record.error)}</p>` : ""}
        ${record.content ? `<div class="runtime-row"><span>归档正文</span><b>${escapeHtml(record.content.slice(0, 2000))}${record.content.length > 2000 ? "…（已截断显示）" : ""}</b></div>` : ""}
        ${record.status === "preview" && record.decision === "publish" ? `<div class="runtime-row"><span>人工审核发布</span><button type="button" class="solid-button" data-action="daily-moment-publish" data-id="${escapeHtml(record.id ?? "")}" ${runtime.dailyMomentRuntimeReady ? "" : "disabled"}>${runtime.dailyMomentRuntimeReady ? "审核并发布" : "发布链尚未验收"}</button></div>` : ""}
        ${record.status === "publish-unknown" ? `<div class="runtime-row"><span>发布结果人工核对</span><span><button type="button" class="solid-button" data-action="daily-moment-resolve-sent" data-id="${escapeHtml(record.id ?? "")}">确认已发布</button> <button type="button" class="solid-button" data-action="daily-moment-resolve-missed" data-id="${escapeHtml(record.id ?? "")}">确认未发布</button></span></div>` : ""}
      </article>`).join("")
    : runtime.dailyMomentArchiveLoaded ? "<p>导入档案中暂无每日内容记录。</p>" : "<p>输入管理令牌后载入受保护的历史归档。</p>";
  const slots = runtime.dailyMomentScheduleSlots.length
    ? `<div class="panel-label">历史计划槽位 · ${runtime.dailyMomentScheduleSlots.length}</div>${runtime.dailyMomentScheduleSlots.map((slot) => `<div class="runtime-row"><span>${escapeHtml(slot.dayKey ?? "日期未知")} · ${slot.at ? new Date(slot.at).toLocaleString("zh-CN") : "时间未记录"}</span><b>${escapeHtml(slot.status ?? "未知")}${slot.reason ? ` · ${escapeHtml(slot.reason)}` : ""}</b></div>`).join("")}`
    : "";
  const generationDecisions = unlinkedGenerationDecisions.length
    ? `<div class="panel-label">Rust 生成账本 · ${unlinkedGenerationDecisions.length}</div>${unlinkedGenerationDecisions.map((item) => `<article class="incident-card">
        <div class="incident-heading"><strong>${escapeHtml(item.dayKey)} · ${escapeHtml(item.slotId)}</strong><span>${item.outcome === "draft" ? "待审核草稿" : "模型决定跳过"}</span></div>
        ${item.decision.reason ? `<p>${escapeHtml(item.decision.reason)}</p>` : ""}
        ${item.decision.content ? `<div class="runtime-row"><span>草稿（尚未发布）</span><b>${escapeHtml(item.decision.content.slice(0, 2000))}${item.decision.content.length > 2000 ? "…（已截断显示）" : ""}</b></div>` : ""}
        ${item.decision.groupSummaries?.length ? `<small>内部群摘要 ${item.decision.groupSummaries.length} 条；此处仅展示公开草稿，不自动发布。</small>` : ""}
      </article>`).join("")}`
    : runtime.dailyMomentArchiveLoaded ? "<p>暂无 Rust 生成草稿或跳过记录。</p>" : "";
  const settings = runtime.dailyMomentSettings;
  const settingsForm = settings ? `<section class="runtime-inline"><div class="panel-label">每日动态配置 · 已导入 Node 参数</div>
    <p>配置保存在 RInBot。运行时启停只控制 Rust 草稿生成与排程，不会自动发布到 QQ 空间；发布仍保持独立关闭门槛。</p>
    <label class="runtime-row"><span>启用 Rust 每日动态草稿生成</span><input id="daily-moment-enabled" type="checkbox" ${settings.enabled ? "checked" : ""} ${!runtime.dailyMomentGenerationAvailable && !settings.enabled ? "disabled" : ""} /></label>
    ${!runtime.dailyMomentGenerationAvailable && !settings.enabled ? "<small>生成运行时未就绪，暂不能启用；已关闭状态可保存其他配置。</small>" : ""}
    <label class="runtime-row"><span>固定时间（上海时区，小时）</span><input id="daily-moment-hour" type="number" min="0" max="23" value="${settings.hour}" /></label>
    <label class="runtime-row"><span>固定时间（分钟）</span><input id="daily-moment-minute" type="number" min="0" max="59" value="${settings.minute}" /></label>
    <label class="runtime-row"><span>固定时间发送间隔（天）</span><input id="daily-moment-interval" type="number" min="1" max="30" value="${settings.intervalDays}" /></label>
    <label class="runtime-row"><span>每群最少消息</span><input id="daily-moment-min-messages" type="number" min="0" max="100" value="${settings.minMessagesPerGroup}" /></label>
    <label class="runtime-row"><span>最多群数</span><input id="daily-moment-max-groups" type="number" min="1" max="50" value="${settings.maxGroups}" /></label>
    <label class="runtime-row"><span>每群最多消息</span><input id="daily-moment-max-messages" type="number" min="5" max="300" value="${settings.maxMessagesPerGroup}" /></label>
    <label class="runtime-row"><span>提示材料上限（字符）</span><input id="daily-moment-max-prompt" type="number" min="20000" max="500000" value="${settings.maxPromptChars}" /></label>
    <label class="runtime-row"><span>启动后窗口内补跑</span><input id="daily-moment-startup-catchup" type="checkbox" ${settings.startupCatchup ? "checked" : ""} /></label>
    <label class="runtime-row"><span>允许使用配图</span><input id="daily-moment-allow-images" type="checkbox" ${settings.allowImages ? "checked" : ""} /></label>
    <label class="runtime-row"><span>最多配图</span><input id="daily-moment-max-images" type="number" min="0" max="4" value="${settings.maxImages}" /></label>
    <label class="runtime-row"><span>空间可见性代码</span><input id="daily-moment-visibility" type="number" value="${settings.visibility}" /></label>
    <label class="runtime-row"><span>目标 QQ 号（逗号分隔）</span><input id="daily-moment-target-uins" value="${escapeHtml(settings.targetUins.join(","))}" /></label>
    <label class="runtime-row"><span>最多联网研究次数</span><input id="daily-moment-research-calls" type="number" min="0" max="10" value="${settings.maxResearchCalls}" /></label>
    <label class="runtime-row"><span>每日动态模型轮数</span><input id="daily-moment-rounds" type="number" min="2" max="16" value="${settings.maxRounds}" /></label>
    <label class="runtime-row"><span>随机时间窗 JSON（空白表示固定时刻）</span><textarea id="daily-moment-windows" rows="4">${escapeHtml(settings.scheduleWindows ? JSON.stringify(settings.scheduleWindows, null, 2) : "")}</textarea></label>
    <button type="button" class="solid-button" data-action="save-daily-moment-settings">保存每日动态配置</button>
    ${runtime.dailyMomentSettingsError ? `<p role="status">${escapeHtml(runtime.dailyMomentSettingsError)}</p>` : ""}
    <small id="daily-moment-settings-status" role="status"></small></section>`
    : runtime.dailyMomentSettingsError ? `<p role="status">每日动态配置：${escapeHtml(runtime.dailyMomentSettingsError)}</p>` : "";
  return `<div class="runtime-inline"><div class="panel-label">每日内容 · 历史档案与待核对</div>
    <small>预览只保存草稿，不发布到 QQ 空间。发布须逐条确认；结果未知时请人工核对，不会重试。</small>
    <label class="runtime-row"><span>管理员令牌（仅用于当前操作）</span><input id="daily-moments-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
    <button type="button" class="solid-button" data-action="daily-moments-archive-load">载入历史归档</button>
    <button type="button" class="solid-button" data-action="daily-moment-run-preview" ${!runtime.dailyMomentGenerationAvailable || runtime.dailyMomentRunInProgress ? "disabled" : ""}>${runtime.dailyMomentRunInProgress ? "正在生成预览…" : "立即生成预览"}</button>
    ${runtime.dailyMomentRunStatus ? `<p role="status">${escapeHtml(runtime.dailyMomentRunStatus)}</p>` : ""}
    ${runtime.dailyMomentArchiveError ? `<p role="status">${escapeHtml(runtime.dailyMomentArchiveError)}</p>` : ""}
    <div class="incident-summary"><span>历史记录 <b>${runtime.dailyMomentRecords.length}</b></span><span>未知发布 <b>${runtime.dailyMomentRecords.filter((item) => item.status === "publish-unknown").length}</b></span><span>计划槽位 <b>${runtime.dailyMomentScheduleSlots.length}</b></span></div>
    <div class="incident-list">${rows}${generationDecisions}${slots}</div>${settingsForm ? `<details><summary>每日内容设置</summary>${settingsForm}</details>` : ""}</div>`;
}
function identityArchiveMarkup(view = "people") {
  const people = runtime.identityPeople.length
    ? runtime.identityPeople.slice(0, 8).map((person) => `<div class="runtime-row"><span>${escapeHtml(String(person.uin ?? "未知 QQ"))} · ${escapeHtml(String(person.primary_name ?? "未命名"))}</span><span><b>${person.is_friend === 1 ? "好友" : "已记录"} · ${Number(person.message_count ?? 0)} 条互动</b>${runtime.config?.identity_management_available ? ` <button type="button" class="solid-button" data-action="identity-profile-edit" data-uin="${escapeHtml(String(person.uin ?? ""))}">编辑档案</button> <button type="button" class="solid-button" data-action="identity-person-merge" data-uin="${escapeHtml(String(person.uin ?? ""))}">合并重复</button>` : ""}</span></div>`).join("")
    : `<p>没有已导入的人物身份记录。</p>`;
  const requestStatus: Record<string, string> = {
    pending: "待处理",
    approved: "已同意",
    rejected: "已拒绝",
    ignored: "已忽略",
    expired: "已过期",
    deciding: "处理中",
    failed: "处理失败",
    held_unknown: "结果未知 · 待核对",
    archived_pending: "旧申请 · 待处理（只读）",
    archived_approved: "旧申请 · 已同意（只读）",
    archived_rejected: "旧申请 · 已拒绝（只读）",
    archived_accepted: "旧申请 · 已通过（只读）",
    archived_ignored: "旧申请 · 已忽略（只读）",
    archived_expired: "旧申请 · 已过期（只读）",
    archived_held_unknown: "旧申请 · 结果未知（只读）",
    archived_failed: "旧申请 · 处理失败（只读）",
  };
  const requests = runtime.identityFriendRequests.length
    ? runtime.identityFriendRequests.slice(0, 8).map((request) => {
      const status = String(request.status ?? "");
      const userId = String(request.userId ?? request.uin ?? "未知 QQ");
      const whitelistApplied = request.whitelistApplied === true || request.whitelist_applied === true;
      const notificationState = String(request.notificationState ?? "pending");
      const controls = runtime.config?.identity_decisions_available && status === "pending" && request.userId
        ? `<div class="runtime-row"><span>人工审批</span><span><button type="button" class="solid-button" data-action="identity-request-decision" data-id="${escapeHtml(String(request.id ?? ""))}" data-decision="approve">同意</button> <button type="button" class="solid-button" data-action="identity-request-decision" data-id="${escapeHtml(String(request.id ?? ""))}" data-decision="reject">拒绝</button></span></div>`
        : "";
      const whitelistControl = runtime.config?.identity_decisions_available
        && runtime.config.incoming_friend_auto_whitelist
        && status === "approved" && !whitelistApplied
        ? `<div class="runtime-row"><span>私聊白名单同步${request.whitelistError || request.whitelist_error ? ` · ${escapeHtml(String(request.whitelistError ?? request.whitelist_error))}` : " · 尚未完成"}</span><button type="button" class="solid-button" data-action="identity-whitelist-retry" data-id="${escapeHtml(String(request.id ?? ""))}">重试本地同步</button></div>`
        : "";
      const notificationControl = runtime.identityRequestSource !== "rust_identity_module" || status.startsWith("archived_")
        ? ""
        : runtime.config?.identity_decisions_available
        && runtime.config.identity_owner_configured
        && runtime.identityRequestSource === "rust_identity_module"
        && status === "pending"
        && (notificationState === "pending" || notificationState === "failed")
        ? `<div class="runtime-row"><span>管理员通知 · ${notificationState === "failed" ? `发送失败：${escapeHtml(String(request.notificationError ?? ""))}` : "尚待发送"}</span><button type="button" class="solid-button" data-action="identity-notification-retry" data-id="${escapeHtml(String(request.id ?? ""))}">发送/重试通知</button></div>`
        : `<div class="runtime-row"><span>管理员通知</span><b>${notificationState === "sent" ? "已确认发送" : notificationState === "held_unknown" ? "结果未知 · 请人工核对，不会重发" : notificationState === "sending" ? "发送中" : "未发送"}</b></div>`;
      return `<div class="runtime-row"><span>${escapeHtml(userId)} · ${escapeHtml(String(request.comment ?? "无附言"))}</span><b>${escapeHtml((requestStatus[status] ?? status) || "未知状态")}</b></div>${notificationControl}${controls}${whitelistControl}`;
    }).join("")
    : `<p>没有好友申请记录。</p>`;
  const requestSource = runtime.identityRequestSource === "rust_identity_module" ? "Rust 实时记录" : "旧库迁移档案";
  const pending = runtime.identityFriendRequests.some((request) => request.status === "pending" && request.userId);
  const whitelistRetry = runtime.identityFriendRequests.some((request) =>
    request.status === "approved"
    && request.whitelistApplied !== true && request.whitelist_applied !== true
    && runtime.config?.incoming_friend_auto_whitelist);
  const notificationRetry = runtime.identityFriendRequests.some((request) =>
    runtime.identityRequestSource === "rust_identity_module"
    && request.status === "pending"
    && (request.notificationState === "pending" || request.notificationState === "failed")
    && runtime.config?.identity_owner_configured);
  const decisionTools = (pending || whitelistRetry || notificationRetry || runtime.config?.identity_management_available) && (runtime.config?.identity_decisions_available || runtime.config?.identity_management_available)
    ? `<label class="runtime-row"><span>管理员令牌（仅用于本次操作）</span><input id="identity-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><small id="identity-decision-status" role="status"></small>`
    : "";
  const unavailable = runtime.config?.observe_only
      ? "当前为观察模式，好友申请只读。"
      : "同意申请后按配置同步私聊白名单；结果未知不会自动重试，白名单同步失败可人工重试。";
  const peopleSource = runtime.identityPeopleSource === "rust_identity_module" ? "实时索引" : "已导入档案（只读）";
  return `<div class="runtime-inline">${view === "people" ? `<div class="panel-label">人物身份 · ${peopleSource}</div>${people}` : `<div class="panel-label">好友申请 · ${requestSource}</div>${requests}<small>${unavailable}</small>`}${decisionTools}</div>`;
}

function memoryConsolidationMarkup() {
  const status = runtime.memoryConsolidationStatus;
  if (!status) return `<div class="runtime-inline"><div class="panel-label">记忆整理</div><p>正在读取整理模块状态…</p></div>`;
  const history = runtime.memoryConsolidationHistory;
  const mutationHistory = runtime.modelMemoryMutationHistory;
  const nodeBackupsMarkup = history?.nodeBackups?.length
    ? `<div class="runtime-inline"><div class="panel-label">Node 原始整理前快照 · ${history.nodeBackups.length} 份</div><p>这些是迁移前保存的完整人物快照。恢复会追加新的全局版本，保留现有历史；恢复范围包含快照记录的全部会话来源。</p>${history.nodeBackups.map((backup) => {
      const reason = backup.reason === "consolidation" ? "整理前自动备份" : backup.reason === "manual-delete" ? "人工删除前备份" : backup.reason === "merge-absorb" ? "人物合并前备份" : "历史备份";
      return `<article class="incident-card"><div class="incident-heading"><strong>${reason}</strong><span>${backup.backedUpAt ? new Date(backup.backedUpAt).toLocaleString("zh-CN") : "时间未知"}</span></div><p>${backup.impressions.length ? backup.impressions.map((item) => escapeHtml(item.content)).join("；") : "（空印象）"}</p><div class="incident-meta"><span>${backup.impressions.length} 条印象 · ${escapeHtml(backup.sourceChatKey || "全局快照")}</span><button type="button" class="solid-button" data-action="memory-consolidation-node-backup-restore" data-backup-key="${escapeHtml(backup.backupKey)}">恢复此 Node 快照</button></div></article>`;
    }).join("")}</div>`
    : "";
  const historyMarkup = history
    ? `<div class="runtime-inline"><div class="panel-label">人物全局版本历史 · QQ ${escapeHtml(history.userId)}</div><p>当前操作会话：${escapeHtml(history.chatKey)}。下方版本属于该 QQ 号的跨会话全局人物印象；此会话用于定位人物，不代表版本仅作用于本群。</p>${history.revisions.length ? history.revisions.slice().reverse().map((revision) => `<article class="incident-card"><div class="incident-heading"><strong>${revision.legacySnapshot ? "Node 导入前快照" : `版本 #${revision.revision}`}</strong><span>${revision.createdAt ? new Date(revision.createdAt).toLocaleString("zh-CN") : "时间未知"}</span></div><p>${revision.impressions.length ? revision.impressions.map((item) => escapeHtml(item)).join("；") : "（空印象）"}</p><div class="incident-meta"><span>${revision.sourceMessageCount} 条来源消息</span>${revision.revision < history.currentRevision ? `<button type="button" class="solid-button" data-action="memory-consolidation-restore" data-revision="${revision.revision}">恢复此版本</button>` : "<span>当前版本</span>"}</div></article>`).join("") : "<p>该 QQ 号尚无全局整理版本。</p>"}${nodeBackupsMarkup}</div>`
    : "";
  const mutationMarkup = mutationHistory
    ? `<div class="runtime-inline"><div class="panel-label">模型记忆变更 · ${escapeHtml(mutationHistory.chatKey)} · QQ ${escapeHtml(mutationHistory.userId || "全部相关人物")}</div>${mutationHistory.mutations.length ? mutationHistory.mutations.map((mutation) => {
      const people = Array.isArray(mutation.before) ? mutation.before : mutation.before.affected ?? [];
      const entries = people.flatMap((person) => person.entries ?? []);
      const action = mutation.action === "remove" ? "软删除" : mutation.action === "append" ? "新增/更新" : mutation.action === "restore" ? "管理员恢复" : mutation.action;
      const detail = JSON.stringify(mutation.before, null, 2).slice(0, 6000);
      const canRestore = mutation.action === "remove" && !mutation.restored;
      return `<article class="incident-card"><div class="incident-heading"><strong>${escapeHtml(action)} · ${entries.length} 项</strong><span>${mutation.createdAt ? new Date(mutation.createdAt).toLocaleString("zh-CN") : "时间未知"}</span></div><p>操作编号：${escapeHtml(mutation.operationId)}</p><details><summary>查看变更前内容</summary><pre>${escapeHtml(detail || "无明细")}</pre></details><div class="incident-meta"><span>${mutation.restored ? "已恢复" : "保留审计记录"}</span>${canRestore ? `<button type="button" class="solid-button" data-action="model-memory-restore" data-operation-id="${escapeHtml(mutation.operationId)}">恢复此删除</button>` : ""}</div></article>`;
    }).join("") : "<p>该会话暂没有模型记忆变更记录。</p>"}</div>`
    : "";
  const run = runtime.memoryConsolidationRunResult ?? (status.manualRun.state !== "idle" ? status.manualRun : null);
  const runResultMarkup = run
    ? `<div role="status"><p>${run.state === "running" ? `正在整理第 ${run.currentIndex ?? 1} / ${run.requested} 位人物（QQ ${escapeHtml(run.currentUserId ?? "—")}）；离开页面不会中断任务。` : `${run.state === "cancelled" ? "整理已停止" : "批量整理完成"}：请求 ${run.requested} 人，成功 ${run.completed} 人，跳过 ${run.skipped.length} 人，失败 ${run.failed.length} 人。`}</p>${run.results.map((item) => `<p>QQ ${escapeHtml(item.userId)}：版本 #${item.revision} 已保存；读取 ${item.sourceMessageCount} 条本人消息，印象 ${item.previousCount} → ${item.impressionCount} 条。</p>`).join("")}${[...run.skipped, ...run.failed].map((item) => `<p>QQ ${escapeHtml(item.userId)}：${escapeHtml(item.reason)}</p>`).join("")}</div>`
    : "";
  return `<div class="runtime-inline"><div class="panel-label">人物记忆整理 · ${status.manualReady ? "手动运行已验收" : status.executorAvailable ? "执行器可用，运行门槛未通过" : "模型执行器未就绪"}</div>
    <div class="incident-summary"><span>功能开关 <b>${status.enabled ? "已启用" : "已停用"}</b></span><span>历史版本库 <b>${status.revisionStoreAvailable ? "可用" : "未挂载"}</b></span><span>自动调度 <b>${status.automaticSchedulingReady ? "就绪" : "未完成"}</b></span></div>

    <small>整理会将所选人物的近期消息发送给已配置的模型，并追加人物全局记忆版本。</small>
    <label class="runtime-row"><span>会话（group:群号 / private:QQ号）</span><input id="memory-consolidation-chat" inputmode="text" placeholder="group:123456" /></label>
    <label class="runtime-row"><span>人物 QQ 号（可用逗号或空格分隔；留空时整理当前会话全部人物）</span><input id="memory-consolidation-user" inputmode="numeric" placeholder="留空即整理全部人物" /></label>
    <label class="runtime-row"><span>管理员令牌（仅用于本次操作）</span><input id="memory-consolidation-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
    <button type="button" class="solid-button" data-action="memory-consolidation-run" ${status.manualReady && status.enabled ? "" : "disabled"}>批量整理所选人物</button>
    <button type="button" class="solid-button" data-action="memory-consolidation-run-all" ${status.manualReady && status.enabled ? "" : "disabled"}>整理当前会话全部人物</button>
    <button type="button" class="solid-button" data-action="memory-consolidation-history" ${status.revisionStoreAvailable ? "" : "disabled"}>读取版本历史</button>
    <button type="button" class="solid-button" data-action="model-memory-history" ${status.revisionStoreAvailable ? "" : "disabled"}>读取模型记忆变更</button>
    ${runtime.memoryConsolidationRunError ? `<p role="status">${escapeHtml(runtime.memoryConsolidationRunError)}</p>` : ""}
    ${runResultMarkup}
    ${runtime.memoryConsolidationHistoryError ? `<p role="status">${escapeHtml(runtime.memoryConsolidationHistoryError)}</p>` : ""}
    ${historyMarkup}
    ${runtime.modelMemoryMutationHistoryError ? `<p role="status">${escapeHtml(runtime.modelMemoryMutationHistoryError)}</p>` : ""}
    ${mutationMarkup}
    ${history || mutationHistory ? "<small>恢复仅在功能停用时允许；恢复影响人物跨会话记忆，并保留旧版本。</small>" : ""}
    </div>`;
}

function relationshipArchiveMarkup(view = "relationships") {
  const status = runtime.relationshipStatus;
  const settings = runtime.relationshipSettings;
  if (!status) return `<div class="runtime-inline"><div class="panel-label">关系试点</div><p>正在读取关系账本…</p></div>`;
  const stateLabel = !status.enabled ? "已停用 · 保留历史数据" : status.active ? "运行中 · 影子评估" : "已启用 · 当前未运行";
  const rows = runtime.relationships.length
    ? runtime.relationships.map((person) => `<article class="incident-card">
        <div class="incident-heading"><strong>QQ ${escapeHtml(person.uin)}</strong><span>${person.chatCount} 个会话 · ${person.messageCount} 条消息</span></div>
        <div class="incident-meta"><span>熟悉度 ${person.familiarity.toFixed(1)} · 好感 ${person.affinity.toFixed(1)} · 摩擦 ${person.friction.toFixed(1)}</span><span>${person.activeDays} 个活跃日</span></div>
        <div class="incident-meta"><span>直接互动 ${person.directInteractions} 次</span><span>最近互动 ${person.lastInteractionAt ? new Date(person.lastInteractionAt).toLocaleString("zh-CN") : "暂无"}</span></div>
        <div class="incident-meta"><span>最近评估 ${person.lastEvaluatedAt ? new Date(person.lastEvaluatedAt).toLocaleString("zh-CN") : "尚未评估"}</span><button type="button" class="solid-button" data-action="relationship-detail" data-uin="${escapeHtml(person.uin)}">查看关系详情</button></div>
      </article>`).join("")
    : `<p>${status.exists ? "账本中暂无关系档案。" : "当前未建立关系账本；启用前须完成迁移与启停验证。"}</p>`;
  const detail = runtime.relationshipDetail;
  const detailMarkup = runtime.relationshipDetailError
    ? `<p role="status">${escapeHtml(runtime.relationshipDetailError)}</p>`
    : detail ? `<article class="incident-card"><div class="panel-label">QQ ${escapeHtml(detail.person.userId ?? detail.person.uin)} · 关系详情</div>
        <div class="incident-meta"><span>熟悉度 ${detail.person.familiarity.toFixed(1)} · 好感 ${detail.person.affinity.toFixed(1)} · 摩擦 ${detail.person.friction.toFixed(1)}</span><span>证据会话 ${Object.keys(detail.cursors).length} 个</span></div>
        <div class="panel-label">开放边界标记 · ${detail.flags.length}</div>${detail.flags.length ? detail.flags.map((flag) => `<div class="runtime-row"><span>${escapeHtml(flag.flagType)}</span><b>${escapeHtml(flag.status)} · ${new Date(flag.createdAt).toLocaleString("zh-CN")}</b></div>`).join("") : "<p>无开放边界标记。</p>"}
        <div class="panel-label">近期关系事件 · ${detail.events.length}</div>${detail.events.length ? detail.events.map((event) => `<div class="runtime-row"><span>${escapeHtml(event.eventType)} · ${event.polarity > 0 ? "正向" : "负向"}</span><b>${escapeHtml(event.summary)} · 置信度 ${event.confidence.toFixed(2)}</b></div>`).join("") : "<p>暂无关系事件。</p>"}
        <div class="panel-label">评估运行记录 · ${detail.evaluationCount}</div>${detail.evaluations.length ? detail.evaluations.map((evaluation) => {
          const usage = evaluation.usage;
          const tokenText = usage.totalTokens !== undefined ? ` · ${usage.totalTokens} tokens${usage.cachedTokens ? `（缓存 ${usage.cachedTokens}）` : ""}` : "";
          const statusText = evaluation.status === "done" ? "完成" : evaluation.status === "running" ? "运行中" : evaluation.status;
          const trace = runtime.relationshipEvaluationTrace?.id === evaluation.id ? runtime.relationshipEvaluationTrace : null;
          return `<div class="runtime-row"><span>${escapeHtml(statusText)} · ${evaluation.evidenceCount} 条证据 · ${escapeHtml(evaluation.model || "沿用主模型")}</span><b>${new Date(evaluation.createdAt).toLocaleString("zh-CN")}${tokenText}${evaluation.hasError ? " · 存在错误（摘要中隐藏）" : ""}<small>${evaluation.sourceChatKeys.map(escapeHtml).join("、") || "未记录会话"}</small><button type="button" class="solid-button" data-action="relationship-evaluation-trace" data-uin="${escapeHtml(detail.person.userId ?? detail.person.uin)}" data-evaluation-id="${escapeHtml(evaluation.id)}">查看模型请求与响应</button>${trace ? `<pre class="relationship-trace">${escapeHtml(JSON.stringify(trace, null, 2))}</pre>` : ""}</b></div>`;
        }).join("") : "<p>暂无评估运行记录。</p>"}
        ${runtime.relationshipEvaluationTraceError ? `<p role="status">${escapeHtml(runtime.relationshipEvaluationTraceError)}</p>` : ""}
        <p>查看完整模型 I/O 需要管理员令牌；令牌仅用于本次读取，不会保存。</p>
        ${detail.evaluationCount > detail.evaluations.length + detail.evaluationOffset ? `<p>仅显示最近 ${detail.evaluations.length} 条，完整记录共 ${detail.evaluationCount} 条。</p>` : ""}
      </article>` : "";
  const settingsForm = settings ? `<div class="runtime-inline relationship-settings">
    <div class="panel-label">关系档案运行参数</div>
    <label class="runtime-row"><span>专用模型（留空沿用主模型）</span><input id="relationship-model" type="text" maxlength="160" value="${escapeHtml(settings.model)}" /></label>
    <label class="runtime-row"><span>新增证据消息阈值（4–50）</span><input data-relationship-setting="minNewMessages" type="number" min="4" max="50" value="${settings.minNewMessages}" /></label>
    <label class="runtime-row"><span>最小批次消息数（2–20）</span><input data-relationship-setting="minBatchMessages" type="number" min="2" max="20" value="${settings.minBatchMessages}" /></label>
    <label class="runtime-row"><span>最小批次年龄（分钟，30–1440）</span><input data-relationship-setting="minBatchAgeMinutes" type="number" min="30" max="1440" value="${settings.minBatchAgeMinutes}" /></label>
    <label class="runtime-row"><span>每人最多证据消息（8–64）</span><input data-relationship-setting="maxEvidenceMessages" type="number" min="8" max="64" value="${settings.maxEvidenceMessages}" /></label>
    <label class="runtime-row"><span>每轮最多人物（1–4）</span><input data-relationship-setting="maxPeoplePerTurn" type="number" min="1" max="4" value="${settings.maxPeoplePerTurn}" /></label>
    <label class="runtime-row"><span>每日评估上限（1–200）</span><input data-relationship-setting="maxEvaluationsPerDay" type="number" min="1" max="200" value="${settings.maxEvaluationsPerDay}" /></label>
    <label class="runtime-row"><span>熟悉度刷新间隔（分钟，1–1440）</span><input data-relationship-setting="familiarityRefreshMinutes" type="number" min="1" max="1440" value="${settings.familiarityRefreshMinutes}" /></label>
    <label class="runtime-row"><span>摩擦半衰期（小时，6–720）</span><input data-relationship-setting="frictionHalfLifeHours" type="number" min="6" max="720" step="0.5" value="${settings.frictionHalfLifeHours}" /></label>
    <label class="runtime-row"><span>最多记忆上下文（0–12）</span><input data-relationship-setting="maxMemoryContext" type="number" min="0" max="12" value="${settings.maxMemoryContext}" /></label>
    <small>运行开关和毕业状态在“功能模块”生命周期页单独管理。参数保存后供运行实例即时使用，历史账本不会因停用而删除。</small>
    <label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="relationship-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
    <button type="button" class="solid-button" data-action="save-relationship-settings">保存并立即生效</button><small id="relationship-settings-status" role="status"></small></div>` : "";
  if (view === "relationship-settings") return settingsForm || "<p>关系评估设置暂不可用。</p>";
  return `<div class="runtime-inline"><div class="panel-label">关系档案 · ${stateLabel}</div>
    <div class="incident-summary"><span>人物 <b>${status.states}</b></span><span>关系事件 <b>${status.events}</b></span><span>边界标记 <b>${status.flags}</b></span><span>评估记录 <b>${status.evaluations}</b></span><span>会话游标 <b>${status.cursors}</b></span></div>
    ${status.error ? `<p role="status">${escapeHtml(status.error)}</p>` : ""}
    <div class="incident-list">${rows}</div>${detailMarkup}${detail ? `<label class="runtime-row"><span>管理员令牌（读取评估详情）</span><input id="relationship-admin-token" type="password" autocomplete="off" /></label>` : ""}</div>`;
}

function groupInviteArchiveMarkup(view = "invites") {
  const entries = runtime.groupInvites;
  const directEntries = runtime.groupInviteDirectEntries;
  const controls = runtime.groupInviteSettings;
  const canManage = Boolean(runtime.config?.api_token_configured);
  const settingsForm = controls
    ? `<div class="runtime-inline relationship-settings">
        <div class="panel-label">群邀请运行设置</div>
        <label class="runtime-row"><span>启用群邀请自动处理</span><input data-group-invite-setting="enabled" type="checkbox" ${controls.settings.enabled ? "checked" : ""} ${!controls.runtimeReady && !controls.settings.enabled ? "disabled" : ""} /></label>
        <label class="runtime-row"><span>仅自动处理私聊白名单中的邀请人</span><input data-group-invite-setting="friendsOnly" type="checkbox" ${controls.settings.friendsOnly ? "checked" : ""} /></label>
        <label class="runtime-row"><span>批准后自动加入聊天白名单</span><input data-group-invite-setting="autoWhitelist" type="checkbox" ${controls.settings.autoWhitelist ? "checked" : ""} /></label>
        <label class="runtime-row"><span>向管理员发送待处理与结果通知</span><input data-group-invite-setting="notifyOwner" type="checkbox" ${controls.settings.notifyOwner ? "checked" : ""} ${controls.ownerConfigured ? "" : "disabled"} /></label>
        ${!controls.runtimeReady ? `<small>运行器未就绪；当前只能保持关闭。请在启动配置启用模块并准备可写账本后重启。</small>` : ""}
        ${!controls.ownerConfigured ? `<small>尚未配置管理员 QQ，管理员通知设置不可用。</small>` : ""}
        ${canManage ? `<label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="group-invite-settings-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><button type="button" class="solid-button" data-action="save-group-invite-settings">保存并立即生效</button><small id="group-invite-settings-status" role="status"></small>` : `<small>设置需要已配置的管理员令牌；只读历史记录仍可查看。</small>`}
      </div>`
    : `<p>群邀请设置正在读取；请刷新页面重试。</p>`;
  const requests = entries.length
    ? entries.map((request) => `<article class="incident-card">
        <div class="incident-heading"><strong>群 ${escapeHtml(request.groupId)} · 邀请人 ${escapeHtml(request.inviterId)}</strong><span>${escapeHtml(request.state)}</span></div>
        <div class="incident-meta"><span>类型 ${escapeHtml(request.subtype)} · ${new Date(request.createdAt).toLocaleString("zh-CN")}</span><span>${escapeHtml(request.decision || "未审批")}</span></div>
        ${request.comment ? `<p>${escapeHtml(request.comment)}</p>` : ""}
        ${request.reviewReason ? `<p role="status">未自动同意：${escapeHtml(request.reviewReason)}</p>` : ""}
        ${request.actionError ? `<p role="status">审批结果：${escapeHtml(request.actionError)}</p>` : ""}
        <div class="incident-meta"><span>群白名单同步：${escapeHtml(request.whitelistState)}${request.whitelistError ? ` · ${escapeHtml(request.whitelistError)}` : ""}</span><span>管理员通知：${escapeHtml(request.notifyState)}${request.notifyError ? ` · ${escapeHtml(request.notifyError)}` : ""}</span></div>
        ${request.notifyState === "failed" ? `<button type="button" class="solid-button" data-action="group-invite-notification-retry" data-id="${escapeHtml(request.id)}">确认重试管理员通知</button>` : request.notifyState === "held_unknown" ? `<small>通知结果未知；请先在 QQ 客户端核对，不提供重发。</small>` : ""}
      </article>`).join("")
    : "";
  const directRows = directEntries.map((entry) => `<article class="incident-card">
      <div class="incident-heading"><strong>机器人直接加入群 ${escapeHtml(entry.groupId)}</strong><span>已记录</span></div>
      <div class="incident-meta"><span>新成员 ${escapeHtml(entry.userId)} · ${new Date(entry.joinedAt * 1000).toLocaleString("zh-CN")}</span><span>操作人 ${escapeHtml(entry.operatorId || "未知")}</span></div>
      <div class="incident-meta"><span>群白名单同步：${escapeHtml(entry.whitelistState)}${entry.whitelistError ? ` · ${escapeHtml(entry.whitelistError)}` : ""}</span><span>管理员通知：${escapeHtml(entry.notifyState)}${entry.notifyError ? ` · ${escapeHtml(entry.notifyError)}` : ""}</span></div>
      ${entry.notifyState === "failed" ? `<button type="button" class="solid-button" data-action="group-invite-direct-notification-retry" data-id="${escapeHtml(entry.id)}">确认重试管理员通知</button>` : entry.notifyState === "held_unknown" ? `<small>通知结果未知；请先在 QQ 客户端核对，不提供重发。</small>` : ""}
    </article>`).join("");
  const allCount = entries.length + directEntries.length;
  const canRetry = Boolean(controls?.settings.notifyOwner)
    && (entries.some((request) => request.notifyState === "failed") || directEntries.some((entry) => entry.notifyState === "failed"));
  if (view === "invite-settings") return settingsForm;
  return `<div class="runtime-inline"><div class="panel-label">群邀请记录 · ${runtime.config?.observe_only ? "观察模式" : "活动模式"}</div>
    <div class="incident-summary"><span>记录 <b>${allCount}</b></span><span>待处理 <b>${entries.filter((item) => item.state === "observed").length}</b></span><span>未知审批 <b>${entries.filter((item) => item.state === "held_unknown").length}</b></span><span>通知失败 <b>${entries.filter((item) => item.notifyState === "failed").length + directEntries.filter((item) => item.notifyState === "failed").length}</b></span></div>
    <div class="incident-list">${requests}${directRows || (!allCount ? `<p>暂无群邀请或直接入群记录。若模块已停用，历史账本仍会保留。</p>` : "")}</div>
    ${canRetry ? `<label class="runtime-row"><span>管理员令牌（仅用于本次通知重试）</span><input id="group-invite-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>` : ""}
    <p>只会自动批准符合群拒绝名单与好友策略的邀请。未知审批或通知结果不会自动重试；只有明确失败的管理员通知可经显式确认重试。</p></div>`;
}

function qzoneInteractionsMarkup(view = "qzone") {
  const settings = runtime.qzoneInteractionsSettings;
  const status = runtime.qzoneInteractionsStatus;
  if (!settings || !status) return `<div class="runtime-inline"><div class="panel-label">空间互动</div><p>正在读取模块状态…</p></div>`;
  const numberFields: Array<[keyof QzoneInteractionsSettings, string, number, number]> = [
    ["feedIntervalMinutes", "动态读取间隔（分钟）", 5, 1440],
    ["replyIntervalMinutes", "评论检查间隔（分钟）", 1, 1440],
    ["feedFetchCount", "每次读取动态数", 1, 50],
    ["ownPostCount", "读取本人动态数", 1, 30],
    ["maxAgeHours", "动态最大年龄（小时）", 1, 720],
    ["maxBatchItems", "每批最大条目", 1, 50],
    ["maxLikesPerRun", "每轮最多点赞", 0, 20],
    ["maxCommentsPerRun", "每轮最多评论", 0, 10],
    ["maxRepliesPerRun", "每轮最多回复", 0, 20],
    ["commentMaxChars", "评论字数上限", 5, 200],
    ["replyMaxChars", "回复字数上限", 5, 200],
    ["actionDelayMinMs", "操作最短间隔（毫秒）", 0, 10000],
    ["actionDelayMaxMs", "操作最长间隔（毫秒）", 0, 15000],
    ["maxDecisionRounds", "单轮模型决策上限", 1, 5],
  ];
  const values = Object.fromEntries(numberFields.map(([key]) => [key, settings[key]]));
  const actionCounts = Object.entries(status.actionStates).map(([name, count]) => `${name} ${count}`).join(" · ") || "暂无待处理操作";
  const audit = runtime.qzoneInteractionsAudit;
  const runRows = audit?.runs.map((run) => `<article class="incident-card"><div class="incident-heading"><strong>运行 ${escapeHtml(run.id ?? "未知编号")}</strong><span>${escapeHtml(run.status ?? "状态未知")}</span></div><div class="incident-meta"><span>决策 ${run.decisionCount ?? 0} · 操作 ${run.actionCount ?? 0}</span><span>完成 ${run.doneCount ?? 0} · 未知 ${run.unknownCount ?? 0}</span></div><small>${run.startedAt ? new Date(run.startedAt).toLocaleString("zh-CN") : "时间未知"}</small></article>`).join("") ?? "";
  const actionRows = audit?.actions.map((action) => `<div class="runtime-row"><span>${escapeHtml(action.action)} · ${escapeHtml(action.itemKind)} · ${escapeHtml(action.itemKey)}</span><b>${escapeHtml(action.state)}${action.error ? ` · ${escapeHtml(action.error)}` : ""}</b></div>`).join("") ?? "";
  return `<div class="runtime-inline"><div class="panel-label">空间互动 · 专属运行页面</div>
    <div class="incident-summary"><span>状态 <b>${status.active ? "运行中" : status.observing ? "只读采集中" : settings.enabled ? "已启用但未运行" : "已停用"}</b></span><span>账本条目 <b>${status.items}</b></span><span>账本 <b>${status.exists ? "已挂载" : "未导入"}</b></span></div>
    ${status.error ? `<p role="status">${escapeHtml(status.error)}</p>` : ""}
    ${!status.runtimeReady ? `<small>外部互动尚未通过验收；当前${status.observing ? "仅采集记录" : "不可执行 QQ 操作"}。</small>` : ""}
    ${view === "qzone" ? `
    <div class="panel-label">外部操作账本</div><p>${escapeHtml(actionCounts)}。unknown 结果只允许人工核对，不会自动重试。</p>
    <section class="runtime-inline"><div class="panel-label">运行记录与操作审计</div><p>仅展示状态与目标索引，不返回外部写入载荷或回执正文；敏感记录需管理员令牌读取。</p>
      <label class="runtime-row"><span>管理员令牌（仅用于载入审核记录）</span><input id="qzi-audit-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="qzone-audit-load">载入最近审核记录</button>
      <label class="runtime-row"><span>手动运行范围</span><select id="qzi-manual-kind"><option value="all">动态与评论</option><option value="feed">仅好友动态</option><option value="reply">仅评论回复</option></select></label>
      <label class="runtime-row"><span>管理员令牌（仅用于本次手动运行）</span><input id="qzi-run-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="qzone-manual-run" ${status.runtimeReady && settings.enabled ? "" : "disabled"}>确认并排入手动运行</button>
      ${runtime.qzoneInteractionsAuditError ? `<p role="status">${escapeHtml(runtime.qzoneInteractionsAuditError)}</p>` : ""}
      <div class="incident-list">${runRows || (runtime.qzoneInteractionsAuditLoaded ? "<p>暂无运行记录。</p>" : "<p>载入后显示互动记录。</p>")}${actionRows ? `<div class="panel-label">最近操作 · ${audit?.actions.length ?? 0} 项</div>${actionRows}` : ""}</div></section>` : `
    <div class="runtime-inline relationship-settings"><div class="panel-label">运行参数（保存后实时应用读取间隔与时段；不会执行 QQ 互动）</div>
      <label class="runtime-row"><span>启动时补读历史动态</span><input id="qzi-startup-catchup" type="checkbox" ${settings.startupCatchup ? "checked" : ""} /></label>
      <label class="runtime-row"><span>活跃时段起始（上海时间；留空为全天）</span><input id="qzi-active-start" type="time" value="${escapeHtml(settings.activeHours?.start ?? "")}" /></label>
      <label class="runtime-row"><span>活跃时段结束</span><input id="qzi-active-end" type="time" value="${escapeHtml(settings.activeHours?.end ?? "")}" /></label>
      ${numberFields.map(([key, label, min, max]) => `<label class="runtime-row"><span>${label}（${min}–${max}）</span><input data-qzi-setting="${key}" type="number" min="${min}" max="${max}" value="${escapeHtml(String(values[key]))}" /></label>`).join("")}
      <label class="runtime-row"><span>允许点赞</span><input id="qzi-allow-likes" type="checkbox" ${settings.allowLikes ? "checked" : ""} /></label>
      <label class="runtime-row"><span>允许评论</span><input id="qzi-allow-comments" type="checkbox" ${settings.allowComments ? "checked" : ""} /></label>
      <label class="runtime-row"><span>允许回复</span><input id="qzi-allow-replies" type="checkbox" ${settings.allowReplies ? "checked" : ""} /></label>
      <label class="runtime-row"><span>管理员令牌（仅用于本次保存）</span><input id="qzi-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="save-qzone-settings">保存运行参数</button><small id="qzi-settings-status" role="status"></small></div>`}
    </div>`;
}

function proactiveMarkup(view = "wakes") {
  const status = runtime.proactiveStatus;
  const settings = runtime.proactiveSettings;
  if (!status) return `<div class="runtime-inline"><div class="panel-label">主动消息</div><p>正在读取模块状态…</p></div>`;
  const counts = Object.entries(status.states).map(([state, count]) => `${escapeHtml(state)} ${count}`).join(" · ") || "暂无唤醒记录";
  const wakes = runtime.proactiveWakes.map((wake) => `<article class="incident-card"><div class="incident-heading"><strong>${escapeHtml(wake.kind === "followUp" ? "补话" : wake.kind === "selfWake" ? "自安排唤醒" : wake.kind)} · ${escapeHtml(wake.conversationId)}</strong><span>${escapeHtml(wake.state)}</span></div><div class="incident-meta"><span>计划时间</span><b>${wake.at ? new Date(wake.at).toLocaleString("zh-CN") : "未设置"}</b></div>${wake.note ? `<p>${escapeHtml(wake.note)}</p>` : ""}${wake.error ? `<small>${escapeHtml(wake.error)}</small>` : ""}${wake.state === "pending" ? `<button class="solid-button" data-action="proactive-wake-cancel" data-id="${escapeHtml(wake.id)}">取消待执行项</button>` : ""}</article>`).join("");
  const settingsMarkup = settings ? `<div class="runtime-inline relationship-settings"><div class="panel-label">运行参数 · 仅配置，不会触发消息</div>
      <label class="runtime-row"><span>检查间隔下限（分钟）</span><input id="proactive-min-interval" type="number" min="1" max="10080" value="${Math.round(settings.checkIntervalMinMs / 60000)}" /></label>
      <label class="runtime-row"><span>检查间隔上限（分钟）</span><input id="proactive-max-interval" type="number" min="2" max="10080" value="${Math.round(settings.checkIntervalMaxMs / 60000)}" /></label>
      <label class="runtime-row"><span>会话空闲门槛（分钟）</span><input id="proactive-idle-threshold" type="number" min="5" max="43200" value="${Math.round(settings.idleThresholdMs / 60000)}" /></label>
      <label class="runtime-row"><span>主动触发概率（0–100%）</span><input id="proactive-probability" type="number" min="0" max="100" value="${Math.round(settings.probability * 100)}" /></label>
      <label class="runtime-row"><span>允许补话</span><input id="proactive-follow-up" type="checkbox" ${settings.followUpEnabled ? "checked" : ""} /></label>
      <label class="runtime-row"><span>允许自安排唤醒</span><input id="proactive-self-wake" type="checkbox" ${settings.selfWakeEnabled ? "checked" : ""} /></label>
      <label class="runtime-row"><span>活跃时段（上海时间；留空为全天）</span><span><input id="proactive-active-start" type="time" value="${escapeHtml(settings.activeHours?.[0]?.start ?? "")}" /> – <input id="proactive-active-end" type="time" value="${escapeHtml(settings.activeHours?.[0]?.end ?? "")}" /></span></label>
      <label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="proactive-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="save-proactive-settings">保存运行参数</button><small id="proactive-settings-status" role="status"></small></div>` : `<div class="runtime-inline"><p>运行设置和唤醒清单受管理令牌保护。输入令牌后载入；令牌仅用于本次请求，不会保存。</p><label class="runtime-row"><span>管理员令牌</span><input id="proactive-load-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><button type="button" class="solid-button" data-action="proactive-load">载入运行设置与唤醒清单</button></div>`;
  return `<div class="runtime-inline"><div class="panel-label">主动消息 · 独立运行页面</div>
    <div class="incident-summary"><span>功能状态 <b>${status.enabled ? status.active ? "运行中" : "已启用但未运行" : "已停用 · 数据保留"}</b></span><span>毕业状态 <b>${status.graduated ? "已毕业" : "试点中"}</b></span><span>调度器 <b>${status.runtimeReady ? "就绪" : "未接通"}</b></span><span>账本 <b>${status.exists ? "已挂载" : "未创建"}</b></span></div>
    ${!status.runtimeReady ? "<small>主动发送尚未通过验收，暂不能启用；历史记录和设置保留。</small>" : ""}
    <div class="panel-label">唤醒账本</div><p>${counts}。unknown 外部发送结果只允许人工核对，绝不自动重试。</p>
    ${runtime.proactiveError ? `<p role="status">${escapeHtml(runtime.proactiveError)}</p>` : ""}${!settings || view === "proactive-settings" ? settingsMarkup : ""}
    ${settings && view === "wakes" ? `<label class="runtime-row"><span>管理员令牌（取消待执行项）</span><input id="proactive-cancel-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><div class="incident-list">${wakes || "<p>暂无唤醒记录。</p>"}</div>` : ""}</div>`;
}

function featureModulesMarkup() {
  const relation = runtime.relationshipSettings, relationStatus = runtime.relationshipStatus;
  const qzone = runtime.qzoneInteractionsSettings, qzoneStatus = runtime.qzoneInteractionsStatus;
  const proactive = runtime.proactiveStatus, sticker = runtime.stickerLifecycle;
  const collection = runtime.stickerCollectionLifecycle, memory = runtime.memoryConsolidationStatus;
  const multimodal = runtime.multimodalContextStatus;
  if (!relation || !relationStatus || !qzone || !qzoneStatus || !proactive || !sticker || !collection || !memory || !multimodal) return "<p>正在读取实验功能状态…</p>";
  const features = [
    { name: "关系评估", enabled: relation.enabled, graduated: relation.graduated, active: relationStatus.active, ready: !relationStatus.error, toggle: "relationship-lifecycle-toggle", graduate: "relationship-graduate" },
    { name: "主动消息", ...proactive, ready: proactive.runtimeReady, toggle: "proactive-lifecycle-toggle", graduate: "proactive-graduate" },
    { name: "人物记忆整理", ...memory, active: memory.enabled && memory.automaticSchedulingReady, ready: memory.runtimeReady, toggle: "memory-consolidation-lifecycle-toggle", graduate: "memory-consolidation-graduate" },
    { name: "多模态上下文", ...multimodal, ready: multimodal.runtimeReady, toggle: "multimodal-context-lifecycle-toggle", graduate: "multimodal-context-graduate" },
    { name: "空间互动", ...qzone, active: qzoneStatus.active, ready: qzoneStatus.runtimeReady, toggle: "qzone-lifecycle-toggle", graduate: "qzone-graduate" },
    { name: "贴纸发送", ...sticker.settings, active: sticker.active, ready: sticker.runtimeReady, toggle: "sticker-lifecycle-toggle", graduate: "sticker-graduate" },
    { name: "贴纸收藏", ...collection.settings, active: collection.active, ready: collection.runtimeReady, toggle: "sticker-collection-lifecycle-toggle", graduate: "sticker-collection-graduate" },
  ];
  return '<div class="runtime-inline"><div class="panel-label">实验功能</div>' + features.map(feature =>
    '<article class="incident-card"><div class="incident-heading"><strong>' + feature.name + '</strong><span>' + (feature.enabled ? feature.active ? "运行中" : "已启用 · 当前未运行" : "已停用") + ' · ' + (feature.graduated ? "已毕业" : "试点中") + '</span></div>' +
    '<div class="incident-actions"><button type="button" class="solid-button" data-action="' + feature.toggle + '" ' + (!feature.enabled && !feature.ready ? "disabled" : "") + '>' + (feature.enabled ? "停用" : "启用") + '</button>' + (feature.graduated ? "" : '<button type="button" class="solid-button" data-action="' + feature.graduate + '" ' + (!feature.ready ? "disabled" : "") + '>确认毕业…</button>') + '</div>' + (!feature.ready ? '<small>运行验收未通过，暂不能启用或毕业。</small>' : "") + '</article>'
  ).join("") + '<label class="runtime-row"><span>管理员令牌</span><input id="feature-admin-token" type="password" autocomplete="off" /></label><small>停用保留配置和数据；毕业状态独立保存，操作需再次确认。</small><small id="feature-lifecycle-status" role="status"></small></div>';
}

async function updateRelationshipLifecycle(action: "toggle" | "graduate") {
  const current = runtime.relationshipSettings;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将关系试点标记为已毕业？毕业状态与运行开关分离；此操作会持久记录。")) return;
  const settings = action === "graduate"
    ? { ...current, graduated: true }
    : { ...current, enabled: !current.enabled };
  const error = await saveRelationshipPilotSettings(settings, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#feature-lifecycle-status");
  if (status) status.textContent = error ?? (action === "graduate" ? "已毕业状态已保存；运行开关保持原样" : settings.enabled ? "模块已启用" : "模块已停用，账本与配置保留");
  notify(error ?? (action === "graduate" ? "毕业状态已保存" : settings.enabled ? "功能已启用" : "功能已停用"));
}

async function updateQzoneLifecycle(action: "toggle" | "graduate") {
  const current = runtime.qzoneInteractionsSettings;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将空间互动标记为已毕业？Rust 运行门槛未完成时此操作会被服务端拒绝。")) return;
  const settings = action === "graduate"
    ? { ...current, graduated: true }
    : { ...current, enabled: !current.enabled };
  const error = await saveQzoneInteractionsSettings(settings, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#feature-lifecycle-status");
  if (status) status.textContent = error ?? (action === "graduate" ? "毕业状态已保存" : settings.enabled ? "模块已启用" : "模块已停用，数据与参数保留");
  if (error) notify(error);
}

async function updateStickerLifecycle(action: "toggle" | "graduate") {
  const current = runtime.stickerLifecycle;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将贴纸功能标记为已毕业？必须先完成发送链和生产验收。")) return;
  const settings = action === "graduate"
    ? { ...current.settings, graduated: true }
    : { ...current.settings, enabled: !current.settings.enabled };
  const error = await saveStickerLifecycle(settings, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#feature-lifecycle-status");
  if (status) status.textContent = error ?? (action === "graduate" ? "毕业状态已保存" : settings.enabled ? "贴纸功能已启用" : "贴纸功能已停用，数据保留");
  if (error) notify(error);
}

async function updateStickerPreferences() {
  const current = runtime.stickerLifecycle;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#sticker-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const promptMaxStickers = Number(document.querySelector<HTMLInputElement>("#sticker-prompt-max")?.value);
  const encourage = Number(document.querySelector<HTMLSelectElement>("#sticker-encourage")?.value);
  const settings = { ...current.settings, promptMaxStickers, encourage };
  const error = await saveStickerLifecycle(settings, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#feature-lifecycle-status");
  if (error) {
    if (status) status.textContent = error;
    notify(error);
  } else {
    if (status) status.textContent = "贴纸模型偏好已保存并立即生效";
    notify("贴纸模型偏好已保存");
  }
}

async function updateStickerCollectionLifecycle(action: "toggle" | "graduate") {
  const current = runtime.stickerCollectionLifecycle;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将贴纸收藏标记为已毕业？自动/手动收藏、QQ 同步和运行验收未完成时服务端会拒绝。")) return;
  const settings = action === "graduate"
    ? { ...current.settings, graduated: true }
    : { ...current.settings, enabled: !current.settings.enabled };
  const error = await saveStickerCollectionLifecycle(settings, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#feature-lifecycle-status");
  if (status) status.textContent = error ?? (action === "graduate" ? "贴纸收藏毕业状态已保存" : settings.enabled ? "贴纸收藏已启用" : "贴纸收藏已停用，数据与设置保留");
  if (error) notify(error);
}

async function updateProactiveFeatureLifecycle(action: "toggle" | "graduate") {
  const current = runtime.proactiveStatus;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将主动消息标记为已毕业？调度、策略、模型和外发链未全部验收时服务端会拒绝。")) return;
  const next = { enabled: action === "graduate" ? current.enabled : !current.enabled, graduated: action === "graduate" ? true : current.graduated };
  const error = await updateProactiveLifecycle(next, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  notify(error ?? (action === "graduate" ? "主动消息毕业状态已保存" : next.enabled ? "主动消息已启用" : "主动消息已停用，数据保留"));
}

async function updateMemoryConsolidationFeatureLifecycle(action: "toggle" | "graduate") {
  const current = runtime.memoryConsolidationStatus;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将人物记忆整理标记为已毕业？自动候选、冷却调度和恢复验收未完成时服务端会拒绝。")) return;
  const next = { enabled: action === "graduate" ? current.enabled : !current.enabled, graduated: action === "graduate" ? true : current.graduated };
  const error = await updateMemoryConsolidationLifecycle(next, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  notify(error ?? (action === "graduate" ? "人物记忆整理毕业状态已保存" : next.enabled ? "人物记忆整理已启用" : "人物记忆整理已停用，数据保留"));
}

async function updateMultimodalContextFeatureLifecycle(action: "toggle" | "graduate") {
  const current = runtime.multimodalContextStatus;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#feature-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (action === "graduate" && !window.confirm("确认将多模态上下文续接标记为已毕业？贴纸图片重读与跨会话续接未验收时服务端会拒绝。")) return;
  const next = { enabled: action === "graduate" ? current.enabled : !current.enabled, graduated: action === "graduate" ? true : current.graduated };
  const error = await updateMultimodalContextLifecycle(next, token, action === "graduate");
  if (tokenInput) tokenInput.value = "";
  notify(error ?? (action === "graduate" ? "多模态上下文毕业状态已保存" : next.enabled ? "多模态上下文已启用" : "多模态上下文已停用，数据保留"));
}

async function persistProactiveSettingsFromForm() {
  const current = runtime.proactiveSettings;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#proactive-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const minMinutes = Number(document.querySelector<HTMLInputElement>("#proactive-min-interval")?.value);
  const maxMinutes = Number(document.querySelector<HTMLInputElement>("#proactive-max-interval")?.value);
  const idleMinutes = Number(document.querySelector<HTMLInputElement>("#proactive-idle-threshold")?.value);
  const probabilityPercent = Number(document.querySelector<HTMLInputElement>("#proactive-probability")?.value);
  const start = document.querySelector<HTMLInputElement>("#proactive-active-start")?.value ?? "";
  const end = document.querySelector<HTMLInputElement>("#proactive-active-end")?.value ?? "";
  if (!Number.isFinite(minMinutes) || !Number.isFinite(maxMinutes) || !Number.isFinite(idleMinutes) || !Number.isFinite(probabilityPercent) || minMinutes > maxMinutes || probabilityPercent < 0 || probabilityPercent > 100 || (!!start !== !!end)) {
    notify("请检查数值范围、间隔上下限和成对填写的活跃时段"); return;
  }
  const settings: ProactiveSettings = {
    ...current,
    checkIntervalMinMs: minMinutes * 60_000,
    checkIntervalMaxMs: maxMinutes * 60_000,
    idleThresholdMs: idleMinutes * 60_000,
    probability: probabilityPercent / 100,
    followUpEnabled: document.querySelector<HTMLInputElement>("#proactive-follow-up")?.checked ?? current.followUpEnabled,
    selfWakeEnabled: document.querySelector<HTMLInputElement>("#proactive-self-wake")?.checked ?? current.selfWakeEnabled,
    activeHours: start && end ? [{ start, end }] : null,
  };
  const error = await saveProactiveSettings(settings, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#proactive-settings-status");
  if (status) status.textContent = error ?? "参数已保存；生命周期状态未改变";
  notify(error ?? "主动消息运行参数已保存");
}

async function persistQzoneSettingsFromForm() {
  const current = runtime.qzoneInteractionsSettings;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#qzi-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const settings: QzoneInteractionsSettings = {
    ...current,
    startupCatchup: document.querySelector<HTMLInputElement>("#qzi-startup-catchup")?.checked ?? false,
    activeHours: (() => {
      const start = document.querySelector<HTMLInputElement>("#qzi-active-start")?.value ?? "";
      const end = document.querySelector<HTMLInputElement>("#qzi-active-end")?.value ?? "";
      return start && end ? { start, end } : null;
    })(),
    allowLikes: document.querySelector<HTMLInputElement>("#qzi-allow-likes")?.checked ?? false,
    allowComments: document.querySelector<HTMLInputElement>("#qzi-allow-comments")?.checked ?? false,
    allowReplies: document.querySelector<HTMLInputElement>("#qzi-allow-replies")?.checked ?? false,
  };
  const keys: Array<keyof QzoneInteractionsSettings> = [
    "feedIntervalMinutes", "replyIntervalMinutes", "feedFetchCount", "ownPostCount", "maxAgeHours",
    "maxBatchItems", "maxLikesPerRun", "maxCommentsPerRun", "maxRepliesPerRun", "commentMaxChars",
    "replyMaxChars", "actionDelayMinMs", "actionDelayMaxMs", "maxDecisionRounds",
  ];
  for (const key of keys) {
    const field = document.querySelector<HTMLInputElement>(`[data-qzi-setting="${key}"]`);
    if (field) settings[key] = Number(field.value) as never;
  }
  const error = await saveQzoneInteractionsSettings(settings, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#qzi-settings-status");
  if (status) status.textContent = error ?? "参数已保存；此次保存不会启动互动任务";
  if (error) notify(error);
}

async function persistRelationshipSettingsFromForm() {
  const current = runtime.relationshipSettings;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#relationship-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const number = (key: keyof RelationshipPilotSettings) => Number(document.querySelector<HTMLInputElement>(`[data-relationship-setting="${key}"]`)?.value);
  const settings: RelationshipPilotSettings = {
    enabled: current.enabled,
    graduated: current.graduated,
    model: document.querySelector<HTMLInputElement>("#relationship-model")?.value.trim() ?? "",
    minNewMessages: number("minNewMessages"),
    minBatchMessages: number("minBatchMessages"),
    minBatchAgeMinutes: number("minBatchAgeMinutes"),
    maxEvidenceMessages: number("maxEvidenceMessages"),
    maxPeoplePerTurn: number("maxPeoplePerTurn"),
    maxEvaluationsPerDay: number("maxEvaluationsPerDay"),
    familiarityRefreshMinutes: number("familiarityRefreshMinutes"),
    frictionHalfLifeHours: number("frictionHalfLifeHours"),
    maxMemoryContext: number("maxMemoryContext"),
  };
  const error = await saveRelationshipPilotSettings(settings, token);
  const status = document.querySelector<HTMLElement>("#relationship-settings-status");
  if (status) status.textContent = error ?? "关系试点设置已保存并生效";
  notify(error ?? "关系试点设置已保存并生效");
}

async function handleIdentityRequestDecision(id: string, decision: "approve" | "reject") {
  const verb = decision === "approve" ? "同意" : "拒绝";
  if (!window.confirm(`确定${verb} QQ 好友申请吗？这会向 QQ 发送实际审批操作。`)) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#identity-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const result = await decideIncomingFriendRequest(id, decision, "", token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#identity-decision-status");
  if ("error" in result) {
    if (status) status.textContent = result.error;
    notify(result.error);
  } else {
    if (status) status.textContent = result.note;
    notify(result.note);
    await new Promise((resolve) => setTimeout(resolve, 150));
    await refreshRuntime();
  }
}
async function handleIdentityWhitelistRetry(id: string) {
  if (!window.confirm("将已确认同意的好友加入私聊白名单。此操作仅修改 RInBot 本地策略，不会再次调用 QQ。")) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#identity-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const result = await retryPrivateWhitelist(id, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#identity-decision-status");
  if ("error" in result) {
    if (status) status.textContent = result.error;
    notify(result.error);
  } else {
    const note = "私聊白名单已同步";
    if (status) status.textContent = note;
    notify(note);
    await refreshRuntime();
  }
}
async function handleIdentityNotificationRetry(id: string) {
  if (!window.confirm("将向配置的管理员 QQ 发送好友申请通知。若发送结果未知，系统会停止并要求人工核对。继续吗？")) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#identity-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const result = await retryFriendRequestNotification(id, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#identity-decision-status");
  if ("error" in result) {
    if (status) status.textContent = result.error;
    notify(result.error);
  } else {
    const note = result.request.notificationState === "sent"
      ? "管理员通知已确认发送"
      : result.request.notificationState === "held_unknown"
        ? "通知结果未知，请在 QQ 客户端核对；不会自动重发"
        : "通知状态已更新";
    if (status) status.textContent = note;
    notify(note);
    await refreshRuntime();
  }
}
async function handleGroupInviteNotificationRetry(id: string, direct = false) {
  if (!window.confirm("将向配置的管理员 QQ 发送群接入通知。结果未知时不会自动重发，请在 QQ 客户端核对。继续吗？")) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#group-invite-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const route = direct
    ? `/api/group-invites/direct/${encodeURIComponent(id)}/notification/retry`
    : `/api/group-invites/${encodeURIComponent(id)}/notification/retry`;
  const response = await apiFetch(route, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
    body: JSON.stringify({ confirm: true }),
    credentials: "same-origin",
  });
  if (tokenInput) tokenInput.value = "";
  const result = await response.json().catch(() => ({})) as { error?: string; request?: { notifyState?: string }; entry?: { notifyState?: string } };
  if (!response.ok) {
    notify(result.error ?? `通知重试失败（HTTP ${response.status}）`);
  } else {
    const notifyState = direct ? result.entry?.notifyState : result.request?.notifyState;
    const note = notifyState === "sent"
      ? "群邀请管理员通知已确认发送"
      : notifyState === "held_unknown"
        ? "通知结果未知，请在 QQ 客户端核对；不会自动重发"
        : "群邀请通知状态已更新";
    notify(note);
  }
  await refreshRuntime();
}
async function handleGroupInviteSettingsSave() {
  const current = runtime.groupInviteSettings;
  if (!current) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#group-invite-settings-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const settings: GroupInviteSettings = {
    enabled: document.querySelector<HTMLInputElement>('[data-group-invite-setting="enabled"]')?.checked ?? false,
    friendsOnly: document.querySelector<HTMLInputElement>('[data-group-invite-setting="friendsOnly"]')?.checked ?? false,
    autoWhitelist: document.querySelector<HTMLInputElement>('[data-group-invite-setting="autoWhitelist"]')?.checked ?? false,
    notifyOwner: document.querySelector<HTMLInputElement>('[data-group-invite-setting="notifyOwner"]')?.checked ?? false,
  };
  if (settings.enabled !== current.settings.enabled && !window.confirm(settings.enabled
    ? "启用后符合条件的群邀请会向 QQ 执行实际批准；确认运行器已验收并继续？"
    : "停用后新邀请不会自动批准，群邀请历史账本会保留。确认停用？")) return;
  const error = await saveGroupInviteSettings(settings, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#group-invite-settings-status");
  if (status) status.textContent = error ?? "群邀请设置已保存并立即生效";
  notify(error ?? "群邀请设置已保存并立即生效");
}
async function handleIdentityProfileEdit(uin: string) {
  const person = runtime.identityPeople.find((entry) => String(entry.uin ?? "") === uin);
  if (!person) { notify("档案已更新，请刷新后重试"); return; }
  const primaryName = window.prompt("设置人物显示名称（留空将使用自动识别名称）", String(person.primary_name ?? ""));
  if (primaryName === null) return;
  const oldProfile = typeof person.profile_json === "string" ? person.profile_json : "{}";
  const profileText = window.prompt("编辑人物备注（JSON 对象）", oldProfile || "{}");
  if (profileText === null) return;
  let profile: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(profileText);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("档案必须是 JSON 对象");
    profile = parsed as Record<string, unknown>;
  } catch (error) {
    notify(error instanceof Error ? error.message : "档案格式无效");
    return;
  }
  const tokenInput = document.querySelector<HTMLInputElement>("#identity-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  if (!window.confirm(`确认保存 QQ ${uin} 的人物档案吗？`)) return;
  const error = await updateIdentityProfile(uin, { primaryName: primaryName.trim(), profile }, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#identity-decision-status");
  if (status) status.textContent = error ?? "人物档案已保存";
  notify(error ?? "人物档案已保存");
}
async function handleIdentityPersonMerge(sourceUin: string) {
  const targetUin = window.prompt(`将 QQ ${sourceUin} 合并到哪个 QQ 号？目标档案中的手动名称和备注优先保留。`);
  if (!targetUin?.trim()) return;
  const normalizedTarget = targetUin.trim();
  if (!/^\d{1,15}$/.test(normalizedTarget) || normalizedTarget === sourceUin) {
    notify("请填写另一个有效 QQ 号");
    return;
  }
  if (!window.confirm(`合并 QQ ${sourceUin} → ${normalizedTarget}？统计、别名、旧记忆与好友申请历史会并入目标；来源档案会保留为隐藏的合并记录，之后的新消息归入目标。`)) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#identity-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const error = await mergeIdentityPerson(sourceUin, normalizedTarget, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#identity-decision-status");
  if (status) status.textContent = error ?? "人物记录已合并";
  notify(error ?? "人物记录已合并");
}
async function handleIncidentAction(
  id: string,
  action: "acknowledge" | "resolve" | "delete" | "notification/retry",
) {
  let body: { resolution?: string; confirm?: boolean } = {};
  if (action === "acknowledge") {
    if (!window.confirm("确认将此异常标记为已知悉？")) return;
  } else if (action === "resolve") {
    const resolution = window.prompt("填写处理结果（必填）");
    if (!resolution?.trim()) return;
    body = { resolution: resolution.trim() };
  } else if (action === "delete") {
    if (!window.confirm("确定删除这条已解决的异常记录？此操作无法撤销。")) return;
    body = { confirm: true };
  } else {
    if (!window.confirm("仅明确失败的告警可以重发。请先检查管理员 QQ；结果未知的告警不会重试。继续发送？")) return;
    body = { confirm: true };
  }
  const tokenInput = document.querySelector<HTMLInputElement>("#incident-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const error = await updateIncident(id, action, token, body);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#incident-action-status");
  if (status) status.textContent = error ?? "异常记录已更新";
  notify(error ?? "异常记录已更新");
}

async function handleIncidentLifecycle(graduate = false) {
  const status = runtime.incidentStatus;
  if (!status) return;
  const enabled = graduate ? status.enabled : !status.enabled;
  const graduated = graduate ? true : status.graduated;
  const confirmation = graduate
    ? "确认将异常处理功能标记为已毕业？毕业状态不可直接撤销。"
    : enabled
      ? "确认启用异常处理与管理员告警？"
      : "确认停用异常处理？历史异常台账会保留，正在进行的告警仍会记录最终结果。";
  if (!window.confirm(confirmation)) return;
  if (graduate && status.graduated) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#incident-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const error = await updateIncidentLifecycle({ enabled, graduated }, token, graduate);
  if (tokenInput) tokenInput.value = "";
  const statusElement = document.querySelector<HTMLElement>("#incident-action-status");
  if (statusElement) statusElement.textContent = error ?? "异常处理状态已更新";
  notify(error ?? "异常处理状态已更新");
}

async function handleIncidentOperationalSettings(button: HTMLElement) {
  const form = button.closest<HTMLElement>(".incident-operational-settings");
  if (!form) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#incident-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const duplicateWindowMinutes = Number(form.querySelector<HTMLInputElement>("[name=duplicateWindowMinutes]")?.value);
  const retentionDays = Number(form.querySelector<HTMLInputElement>("[name=retentionDays]")?.value);
  if (!Number.isInteger(duplicateWindowMinutes) || duplicateWindowMinutes < 1 || duplicateWindowMinutes > 1440) {
    notify("异常去重窗口须为 1–1440 分钟");
    return;
  }
  if (!Number.isInteger(retentionDays) || retentionDays < 1 || retentionDays > 3650) {
    notify("已解决记录保留期限须为 1–3650 天");
    return;
  }
  const settings = {
    notifyWarnings: Boolean(form.querySelector<HTMLInputElement>("[name=notifyWarnings]")?.checked),
    duplicateWindowMinutes,
    retentionDays,
    unknownWritesBlockChat: Boolean(form.querySelector<HTMLInputElement>("[name=unknownWritesBlockChat]")?.checked),
  };
  if (settings.unknownWritesBlockChat && !runtime.incidentSettings?.unknownWritesBlockChat
    && !window.confirm("启用后，存在未核对外发结果的会话将暂停处理新消息。确认启用？")) return;
  const error = await updateIncidentOperationalSettings(settings, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#incident-action-status");
  if (status) status.textContent = error ?? "异常处理参数已保存";
  notify(error ?? "异常处理参数已保存");
}

async function handleChatControlSave(button: HTMLElement) {
  const form = button.closest<HTMLElement>(".chat-control-form");
  if (!form) return;
  const creating = form.dataset.create === "true";
  const chatKey = creating
    ? form.querySelector<HTMLInputElement>("[name=chatKey]")?.value.trim() ?? ""
    : form.dataset.chatKey ?? "";
  if (!/^(group|private):\d{1,15}$/.test(chatKey)) { notify("会话标识格式应为 group:群号 或 private:QQ号"); return; }
  const mode = form.querySelector<HTMLSelectElement>("[name=mode]")?.value ?? "auto";
  const reason = form.querySelector<HTMLInputElement>("[name=reason]")?.value.trim() ?? "";
  const backlogAction = form.querySelector<HTMLSelectElement>("[name=backlogAction]")?.value ?? "keep";
  if (mode === "continue" && !window.confirm("允许该会话继续处理新消息，但不会重发未知外部写入。确认继续？")) return;
  if (backlogAction === "discard" && !window.confirm("将归档并丢弃该会话当前暂停的积压消息；操作不可撤销。继续？")) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#incident-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const expectedVersion = Number(form.dataset.version ?? "0");
  const error = await saveChatControl(chatKey, {
    mode, reason, expectedVersion, backlogAction, confirm: mode === "continue",
  }, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#incident-action-status");
  if (status) status.textContent = error ?? "会话控制已更新";
  notify(error ?? "会话控制已更新");
}

function incidentLedgerMarkup(view = "incidents") {
  const status = runtime.incidentStatus;
  if (!status) return `<div class="runtime-inline"><div class="panel-label">异常台账</div><p>正在读取异常记录…</p></div>`;
  const severity: Record<string, string> = { info: "提示", warning: "警告", error: "错误", critical: "严重" };
  const state: Record<string, string> = { open: "待处理", acknowledged: "已知悉", resolved: "已解决" };
  const notice: Record<string, string> = { none: "无需通知", pending: "待发送", sending: "发送中", sent: "已确认发送", failed: "明确失败", unknown: "结果未知 · 禁止重发" };
  const canManageLifecycle = Boolean(runtime.config?.api_token_configured);
  const canWrite = status.enabled && canManageLifecycle;
  const canRetryAlert = Boolean(canWrite && !runtime.config?.observe_only);
  const operationalSettings = runtime.incidentSettings;
  const items = runtime.incidents.length
    ? runtime.incidents.map((incident) => `<article class="incident-card">
      <div class="incident-card-head"><strong>${incident.category === "agent_feedback" ? "模型反馈" : escapeHtml(incident.code)}</strong><span>${incident.category === "agent_feedback" ? `反馈级别：${escapeHtml(String(incident.details?.reportedLevel ?? "info"))}` : severity[incident.severity] ?? escapeHtml(incident.severity)} · ${state[incident.state] ?? escapeHtml(incident.state)}</span></div>
      ${incident.category === "agent_feedback" ? "" : `<p class="incident-note">${escapeHtml(incident.errorNote || "暂无预置中文说明，请结合原始结果和会话上下文排查。")}</p>`}
      <p>${escapeHtml(incident.message)}</p>
      <div class="incident-meta"><span>${escapeHtml(incident.source)}${incident.chatKey ? ` · ${escapeHtml(incident.chatKey)}` : " · 全局"}</span><span>${new Date(incident.lastAt).toLocaleString("zh-CN")} · 累计 ${incident.count} 次</span></div>
      <div class="incident-meta"><span>管理员告警：${notice[incident.notifyState] ?? escapeHtml(incident.notifyState)}</span><span>${escapeHtml(incident.notifyError || incident.resolution || "")}</span></div>
      ${canWrite ? `<div class="incident-actions">${incident.state === "open" ? `<button class="solid-button" data-action="incident-acknowledge" data-id="${escapeHtml(incident.id)}">标记已知悉</button>` : ""}${incident.state !== "resolved" ? `<button class="solid-button" data-action="incident-resolve" data-id="${escapeHtml(incident.id)}">填写处理结果</button>` : `<button class="solid-button" data-action="incident-delete" data-id="${escapeHtml(incident.id)}">删除记录</button>`}${canRetryAlert && incident.notifyState === "failed" ? `<button class="solid-button" data-action="incident-notification-retry" data-id="${escapeHtml(incident.id)}">确认后重试告警</button>` : ""}</div>` : ""}
    </article>`).join("")
    : `<p>暂无异常记录。</p>`;
  const tokenField = canManageLifecycle
    ? `<label class="runtime-row"><span>管理令牌（仅用于本次操作）</span><input id="incident-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><small id="incident-action-status" role="status"></small>`
    : "";
  const lifecycleActions = canManageLifecycle
    ? `<div class="incident-actions">${status.enabled ? `<button class="solid-button" data-action="incident-lifecycle-toggle">停用异常处理</button>` : `<button class="solid-button" data-action="incident-lifecycle-toggle">启用异常处理</button>`}${status.graduated ? `<span>已毕业</span>` : `<button class="solid-button" data-action="incident-graduate">确认毕业…</button>`}</div>`
    : `<p>设置异常处理生命周期需要已配置的管理员令牌。</p>`;
  const operationalSettingsForm = canManageLifecycle && status.exists && operationalSettings
    ? `<div class="incident-operational-settings incident-card">
        <div class="panel-label">异常处理运行参数</div>
        <label class="runtime-row"><span>异常告警（warning）</span><input name="notifyWarnings" type="checkbox" ${operationalSettings.notifyWarnings ? "checked" : ""} /></label>
        <label class="runtime-row"><span>重复事件合并窗口（分钟）</span><input name="duplicateWindowMinutes" type="number" min="1" max="1440" value="${operationalSettings.duplicateWindowMinutes}" /></label>
        <label class="runtime-row"><span>已解决记录保留（天；清理由下次启动执行）</span><input name="retentionDays" type="number" min="1" max="3650" value="${operationalSettings.retentionDays}" /></label>
        <label class="runtime-row"><span>未核对外发结果时暂停该会话</span><input name="unknownWritesBlockChat" type="checkbox" ${operationalSettings.unknownWritesBlockChat ? "checked" : ""} /></label>
        <button class="solid-button" data-action="incident-settings-save">保存异常处理参数</button>
      </div>`
    : `<p>运行参数需先建立可写异常台账，并配置管理令牌。</p>`;
  const modeOptions = (selected: string) => `<option value="auto" ${selected === "auto" ? "selected" : ""}>自动</option><option value="blocked" ${selected === "blocked" ? "selected" : ""}>阻塞</option><option value="continue" ${selected === "continue" ? "selected" : ""}>继续（不重试旧写入）</option>`;
  const backlogOptions = `<option value="keep">保留积压，等待后续消息</option><option value="recent">立即处理最近 100 条</option><option value="discard">归档并丢弃积压</option>`;
  const controls = runtime.chatControls.length
    ? runtime.chatControls.map(({ control, decision, held }) => `<div class="chat-control-form incident-card" data-chat-key="${escapeHtml(control.chatKey)}" data-version="${control.version}">
        <div class="incident-card-head"><strong>${escapeHtml(control.chatKey)}</strong><span>${control.mode === "auto" ? "自动" : control.mode === "blocked" ? "阻塞" : "继续"} · ${decision.allowed ? "允许新消息" : "当前阻塞"}</span></div>
        <div class="incident-meta"><span>${held ? `未核对外发 ${held} 项` : "没有待核对外发"}</span><span>${escapeHtml(decision.reason || "会话正常")}</span></div>
        <label class="runtime-row"><span>运行模式</span><select name="mode">${modeOptions(control.mode)}</select></label>
        <label class="runtime-row"><span>原因备注</span><input name="reason" maxlength="300" value="${escapeHtml(control.reason)}" /></label>
        <label class="runtime-row"><span>恢复时积压策略</span><select name="backlogAction">${backlogOptions}</select></label>
        ${canWrite ? `<button class="solid-button" data-action="save-chat-control">保存会话控制</button>` : ""}
      </div>`).join("")
    : `<p>暂无已设置的会话控制。可按下方格式新增群聊或私聊控制。</p>`;
  const newControl = canWrite ? `<div class="chat-control-form incident-card" data-create="true" data-version="0">
      <div class="panel-label">新增会话控制</div>
      <label class="runtime-row"><span>会话标识</span><input name="chatKey" placeholder="group:群号 或 private:QQ号" /></label>
      <label class="runtime-row"><span>运行模式</span><select name="mode">${modeOptions("auto")}</select></label>
      <label class="runtime-row"><span>原因备注</span><input name="reason" maxlength="300" /></label>
      <label class="runtime-row"><span>恢复时积压策略</span><select name="backlogAction">${backlogOptions}</select></label>
      <button class="solid-button" data-action="save-chat-control">添加会话控制</button>
    </div>` : "";
  if (view === "incident-lifecycle") return `<div class="incident-card"><div class="incident-heading"><strong>异常处理</strong><span>${status.enabled ? "已启用" : "已停用"} · ${status.graduated ? "已毕业" : "试点中"}</span></div>${lifecycleActions}${tokenField}</div>`;
  if (view === "incident-settings") return `<div class="runtime-inline">${operationalSettingsForm}${tokenField}</div>`;
  if (view === "chat-controls") return `<div class="runtime-inline"><div class="panel-label">会话运行控制</div><small>暂停时保留消息积压。恢复须选择积压处理方式，不会重发未知外发。</small>${tokenField}<div class="incident-list">${controls}${newControl}</div></div>`;
  return `<div class="runtime-inline"><div class="panel-label">异常处理 · ${status.enabled ? runtime.config?.observe_only ? "已启用 · 观察模式" : "运行中" : "已停用 · 保留历史数据"}</div>
    <div class="incident-summary"><span>待处理 <b>${status.open}</b></span><span>已知悉 <b>${status.acknowledged}</b></span><span>已解决 <b>${status.resolved}</b></span><span>严重 <b>${status.critical}</b></span><span>待发告警 <b>${status.pendingNotifications}</b></span></div>
    ${status.error ? `<p role="status">${escapeHtml(status.error)}</p>` : ""}${tokenField}<div class="incident-list">${items}</div>
    <small>仅明确失败的告警允许确认后重试；未知结果须在 QQ 中核对。</small></div>`;
}

function runtimeMarkup(category: string, title: string, view = "connection") {
  const snapshot = runtime.snapshot;
  if (!snapshot) return `<div class="runtime-inline"><div class="panel-label">RInBot 运行状态</div><p>正在连接本地服务…</p></div>`;
  const status = snapshot.status;
  if (title === "身份与申请") return identityArchiveMarkup();
  if (title === "记忆整理") return memoryConsolidationMarkup();
  if (title === "群邀请管理") return groupInviteArchiveMarkup();
  if (title === "每日内容") return dailyMomentsArchiveMarkup();
  if (title === "会话交接") return memoryHandoffMarkup();
  if (title === "贴纸管理") return stickerArchiveMarkup();
  if (title === "关系档案") return relationshipArchiveMarkup();
  if (title === "空间互动") return qzoneInteractionsMarkup();
  if (title === "主动消息") return proactiveMarkup();
  if (title === "未知外发对账") return unknownOutboxMarkup();
  if (title === "异常台账") return incidentLedgerMarkup();
  if (title === "功能模块" || title === "策略引擎") return featureModulesMarkup();
  if (title === "语音识别") {
    const asr = runtime.asrSettings;
    if (!asr) return `<div class="runtime-inline"><div class="panel-label">语音转写</div><p>正在读取模块设置…</p></div>`;
    const settings = asr.settings;
    return `<div class="runtime-inline"><div class="panel-label">语音转写 · 独立模块</div>
      <div class="runtime-row"><span>运行就绪</span><b>${asr.configured ? "已配置，可供对话模型按需调用" : "未就绪：需配置服务地址、模型和密钥"}</b></div>
      <label class="runtime-row"><span>启用语音转写工具</span><input id="asr-enabled" type="checkbox" ${settings.enabled ? "checked" : ""} /></label>
      <label class="runtime-row"><span>识别服务（仅 OpenAI 兼容已迁移）</span><select id="asr-provider">${[["openai", "OpenAI 兼容接口"], ["volc", "火山引擎 · 待迁移"], ["aliyun", "阿里云 · 待迁移"], ["baidu", "百度 · 待迁移"], ["tencent", "腾讯云 · 待迁移"], ["iflytek", "讯飞 · 待迁移"], ["local", "本地服务 · 待迁移"]].map(([value, label]) => `<option value="${value}" ${settings.provider === value ? "selected" : ""}>${label}</option>`).join("")}</select></label>
      <label class="runtime-row"><span>服务地址</span><input id="asr-base-url" type="url" value="${escapeHtml(settings.base_url)}" placeholder="https://api.example.com/v1" /></label>
      <label class="runtime-row"><span>模型名称</span><input id="asr-model" type="text" maxlength="200" value="${escapeHtml(settings.model)}" /></label>
      <label class="runtime-row"><span>语言提示（可留空自动检测）</span><input id="asr-language" type="text" maxlength="40" value="${escapeHtml(settings.language)}" placeholder="zh" /></label>
      <label class="runtime-row"><span>每小时识别上限（1–200）</span><input id="asr-max-per-hour" type="number" min="1" max="200" value="${settings.max_per_hour}" /></label>
      <label class="runtime-row"><span>请求超时（秒，1–600）</span><input id="asr-timeout" type="number" min="1" max="600" value="${Math.round(settings.timeout_ms / 1000)}" /></label>
      <div class="runtime-row"><span>当前密钥</span><b>${asr.apiKeyConfigured ? "已安全保存 · 不会回显" : "未配置"}</b></div>
      <label class="runtime-row"><span>新 API 密钥（留空保留当前）</span><input id="asr-api-key" type="password" autocomplete="new-password" /></label>
      <label class="runtime-row"><span>明确清除当前密钥</span><input id="asr-clear-api-key" type="checkbox" /></label>
      <small>关闭只停止向模型提供转写工具，媒体和历史数据保留。仅 OpenAI 兼容接口已迁移；选择其他供应商后不能启用，直到相应 Rust 适配器完成并通过验收。</small>
      <label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="asr-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="save-asr-settings">保存并立即生效</button><small id="asr-settings-status" role="status"></small></div>`;
  }
  if (title === "配置档案" && runtime.config) {
    const config = runtime.config;
    const queue = runtime.queueSettings;
    const timeControl = runtime.timeControlSettings;
    const behavior = runtime.behaviorSettings;
    const modelResponse = runtime.modelSettings;
    const webSearch = runtime.webSearchSettings;
    const queueForm = queue ? `<div class="runtime-inline"><div class="panel-label">消息队列设置 · 即时生效</div>
      <label class="runtime-row"><span>防抖最短等待（毫秒）</span><input data-queue-setting="wake_delay_min_ms" type="number" min="0" max="60000" value="${queue.wake_delay_min_ms}" /></label>
      <label class="runtime-row"><span>防抖最长等待（毫秒）</span><input data-queue-setting="wake_delay_max_ms" type="number" min="0" max="60000" value="${queue.wake_delay_max_ms}" /></label>
      <label class="runtime-row"><span>单批最大等待（毫秒）</span><input data-queue-setting="max_batch_wait_ms" type="number" min="100" max="20000" value="${queue.max_batch_wait_ms}" /></label>
      <label class="runtime-row"><span>单批消息上限</span><input data-queue-setting="batch_limit" type="number" min="1" max="100" value="${queue.batch_limit}" /></label>
      <label class="runtime-row"><span>单批字符上限</span><input data-queue-setting="batch_max_chars" type="number" min="1000" max="200000" value="${queue.batch_max_chars}" /></label>
      <label class="runtime-row"><span>全局最大并发会话（1–32）</span><input data-queue-setting="max_concurrent_runs" type="number" min="1" max="32" value="${queue.max_concurrent_runs}" /></label>
      <label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="queue-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="save-queue-settings">保存并立即生效</button><small id="queue-settings-status" role="status"></small></div>` : "";
    const webSearchForm = webSearch ? `<div class="runtime-inline"><div class="panel-label">联网搜索 · Bing</div>
      <label class="runtime-row"><span>启用搜索工具</span><input id="web-search-enabled" type="checkbox" ${webSearch.enabled ? "checked" : ""} /></label>
      <div class="runtime-row"><span>搜索提供方</span><b>${escapeHtml(webSearch.provider)}${webSearch.provider.toLowerCase() === "bing" ? " · 中国站" : " · Rust 当前仅支持 Bing"}</b></div>
      <label class="runtime-row"><span>搜索地址（仅 cn.bing.com）</span><input id="web-search-url" type="url" value="${escapeHtml(webSearch.search_url)}" /></label>
      <label class="runtime-row"><span>每次最多返回结果数（1–10）</span><input id="web-search-max-results" type="number" min="1" max="10" value="${webSearch.max_results}" /></label>
      <small>关闭后保留设置；打开后新消息立即使用。网页抓取仅用于读取公开网页，并会拦截本机/内网地址。</small>
      <label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="web-search-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="save-web-search-settings">保存并立即生效</button><small id="web-search-settings-status" role="status"></small></div>` : "";
    const timeControlForm = timeControl ? timeFormMarkup(timeControl) : "";
    const behaviorForm = behavior ? behaviorFormMarkup(behavior) : "";
    const model = modelResponse?.settings;
    const modelForm = model ? `<div class="runtime-inline"><div class="panel-label">模型与提示词 · OpenAI 兼容服务</div>
      <div class="runtime-row"><span>当前密钥</span><b>${modelResponse.api_key_configured ? "已安全保存 · 不会回显" : "未配置"}</b></div>
      <label class="runtime-row"><span>服务地址（HTTP(S)）</span><input id="model-base-url" type="url" value="${escapeHtml(model.base_url)}" placeholder="https://api.example.com/v1" /></label>
      <label class="runtime-row"><span>模型名称</span><input id="model-id" type="text" maxlength="200" value="${escapeHtml(model.model)}" /></label>
      <label class="runtime-row runtime-json-setting"><span>系统提示词</span><textarea id="model-system-prompt">${escapeHtml(model.system_prompt)}</textarea></label>
      <label class="runtime-row"><span>生成随机性（0–2）</span><input id="model-temperature" type="number" min="0" max="2" step="0.1" value="${model.temperature}" /></label>
      <label class="runtime-row"><span>最大输出词元（1–32768）</span><input id="model-max-tokens" type="number" min="1" max="32768" value="${model.max_tokens}" /></label>
      <label class="runtime-row"><span>单次运行工具轮数（1–64）</span><input id="model-max-tool-rounds" type="number" min="1" max="64" value="${model.max_tool_rounds}" /></label>
      <label class="runtime-row"><span>请求超时（毫秒）</span><input id="model-timeout-ms" type="number" min="1000" max="300000" step="1000" value="${model.timeout_ms}" /></label>
      <label class="runtime-row"><span>新 API 密钥（留空保留当前密钥）</span><input id="model-api-key" type="password" autocomplete="new-password" placeholder="更换服务域名时请填写新密钥" /></label>
      <label class="runtime-row"><span>明确清除当前服务密钥</span><input id="model-clear-api-key" type="checkbox" /></label>
      <small>密钥单独存放于权限 0600 的本地文件，并按服务地址绑定；更换域名时必须输入新密钥或明确清除旧密钥。</small>
      <div class="panel-label">备用模型 · 主模型故障时切换一次</div>
      <label class="runtime-row"><span>启用备用模型</span><input id="model-fallback-enabled" type="checkbox" ${model.fallback.enabled ? "checked" : ""} /></label>
      <div class="runtime-row"><span>备用模型密钥</span><b>${modelResponse.fallback_api_key_configured ? "已安全保存 · 不会回显" : "未单独配置 · 默认沿用主模型密钥"}</b></div>
      <label class="runtime-row"><span>备用服务地址（HTTP(S)）</span><input id="model-fallback-base-url" type="url" value="${escapeHtml(model.fallback.base_url)}" placeholder="留空时沿用主模型地址" /></label>
      <label class="runtime-row"><span>备用模型名称</span><input id="model-fallback-id" type="text" maxlength="200" value="${escapeHtml(model.fallback.model)}" /></label>
      <label class="runtime-row"><span>备用模型超时（毫秒）</span><input id="model-fallback-timeout-ms" type="number" min="1000" max="300000" step="1000" value="${model.fallback.timeout_ms}" /></label>
      <label class="runtime-row"><span>备用服务新 API 密钥（留空保留/沿用主密钥）</span><input id="model-fallback-api-key" type="password" autocomplete="new-password" /></label>
      <label class="runtime-row"><span>明确清除备用服务密钥</span><input id="model-fallback-clear-api-key" type="checkbox" /></label>
      <small>备用密钥单独保存在权限 0600 的本地文件中；备用服务失败后不会递归切换其他模型。</small>
      <div class="panel-label">图片识别模型 · 仅处理图片，不参与文字对话</div>
      <label class="runtime-row"><span>启用独立图片识别</span><input id="model-vision-enabled" type="checkbox" ${model.vision_model.enabled ? "checked" : ""} /></label>
      <div class="runtime-row"><span>图片识别模型密钥</span><b>${modelResponse.vision_api_key_configured ? "已安全保存 · 不会回显" : "未配置"}</b></div>
      <label class="runtime-row"><span>图片识别服务地址</span><input id="model-vision-base-url" type="url" value="${escapeHtml(model.vision_model.base_url)}" placeholder="https://api.example.com/v1" /></label>
      <label class="runtime-row"><span>图片识别模型名称</span><input id="model-vision-id" type="text" maxlength="200" value="${escapeHtml(model.vision_model.model)}" /></label>
      <label class="runtime-row"><span>图片识别超时（毫秒）</span><input id="model-vision-timeout-ms" type="number" min="1000" max="300000" step="1000" value="${model.vision_model.timeout_ms || 90000}" /></label>
      <label class="runtime-row"><span>图片识别服务新 API 密钥（留空保留）</span><input id="model-vision-api-key" type="password" autocomplete="new-password" /></label>
      <label class="runtime-row"><span>明确清除图片识别密钥</span><input id="model-vision-clear-api-key" type="checkbox" /></label>
      <small>图片只发送到此处配置的专用模型；识别失败不会自动改发主对话模型。</small>
      <label class="runtime-row"><span>管理员令牌（只用于本次保存）</span><input id="model-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label>
      <button type="button" class="solid-button" data-action="save-model-settings">保存并立即生效</button><small id="model-settings-status" role="status"></small></div>` : "";
    const forms: Record<string, string> = { "queue-settings": queueForm, "web-search": webSearchForm, schedule: timeControlForm, behavior: behaviorForm, models: modelForm };
    if (view in forms) return forms[view] || "<p>当前设置暂不可用，请检查连接后重试。</p>";
    return `<div class="runtime-inline"><div class="panel-label">服务配置（敏感值已隐藏）</div><div class="runtime-row"><span>服务地址</span><b>${escapeHtml(config.host)}:${config.port}</b></div><div class="runtime-row"><span>数据目录</span><b>${escapeHtml(config.data_dir)}</b></div><div class="runtime-row"><span>OneBot 接入</span><b>${config.connect_onebot ? "已启用" : "未启用"} · ${config.observe_only ? "观察模式" : "活动模式"}</b></div><div class="runtime-row"><span>访问令牌</span><b>控制台 ${config.api_token_configured ? "已配置" : "未配置"} · OneBot ${config.onebot_token_configured ? "已配置" : "未配置"}</b></div><div class="runtime-row"><span>连接状态</span><b>${status.onebot_connected ? "已连接" : "未连接"}</b></div><div class="runtime-row"><span>数据库版本</span><b>${status.migration_version}</b></div><small>监听地址、数据目录及接入方式由启动配置管理。</small></div>`;
  }
  if (title === "任务档案") {
    const stateLabel: Record<string, string> = {
      queued: "排队中", processing: "处理中", held: "已挂起", failed: "失败待查",
    };
    const taskRows = runtime.tasks.length
      ? runtime.tasks.map((task) => `<div class="runtime-row"><span>消息任务 ${task.id} · ${escapeHtml(task.conversation_id)}</span><span><b>${stateLabel[task.state] ?? escapeHtml(task.state)} · 尝试 ${task.attempts}</b>${task.state === "queued" ? ` <button type="button" class="solid-button" data-action="cancel-queued-task" data-task-id="${task.id}">取消</button>` : ""}</span></div>`).join("")
      : `<p>当前没有待处理或待核对的消息任务。</p>`;
    return `<div class="runtime-inline"><div class="panel-label">消息处理任务 · 本页 ${runtime.tasks.length} 项</div>${taskRows}${runtime.tasks.some((task) => task.state === "queued") ? `<label class="runtime-row"><span>管理令牌（取消排队任务）</span><input id="task-admin-token" type="password" autocomplete="off" placeholder="输入控制台管理令牌" /></label><small id="task-action-status" role="status"></small>` : ""}${paginationMarkup("tasks", runtime.taskOffset, runtime.tasks.length)}<small>仅可取消未开始处理的消息任务。</small></div>`;
  }
  const extra = category === "自动化档案"
    ? `<div class="runtime-row"><span>待人工确认的外发记录</span><b>${snapshot.counts.unknown_outbox} · ${runtime.unknown.length ? "需要处理" : "无待处理项"}</b></div><div class="runtime-row"><span>消息处理任务</span><b>${runtime.tasks.length} · 仅展示队列状态</b></div>`
    : category === "资源档案"
      ? `<div class="runtime-row"><span>媒体与三维资源</span><b>按需加载</b></div>`
      : `<div class="runtime-row"><span>已保存消息</span><b>${snapshot.counts.messages}</b></div>`;
  return `<div class="runtime-inline"><div class="panel-label">RInBot 运行状态</div><div class="runtime-row"><span>核心服务</span><b>${status.core_ready ? "正常" : "等待中"}</b></div><div class="runtime-row"><span>OneBot 连接</span><b>${status.onebot_connected ? "已连接" : "观察模式 / 未连接"}</b></div>${extra}</div>`;
}
async function persistQueueSettingsFromForm() {
  const current = runtime.queueSettings;
  if (!current) return;
  const value = (key: keyof QueueSettings) => Number(document.querySelector<HTMLInputElement>(`[data-queue-setting="${key}"]`)?.value);
  const settings: QueueSettings = {
    wake_delay_min_ms: value("wake_delay_min_ms"),
    wake_delay_max_ms: value("wake_delay_max_ms"),
    max_batch_wait_ms: value("max_batch_wait_ms"),
    batch_limit: value("batch_limit"),
    batch_max_chars: value("batch_max_chars"),
    max_concurrent_runs: value("max_concurrent_runs"),
  };
  const token = document.querySelector<HTMLInputElement>("#queue-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const error = await saveQueueSettings(settings, token);
  const status = document.querySelector<HTMLElement>("#queue-settings-status");
  if (status) status.textContent = error ?? "已保存，新消息立即使用此设置";
  notify(error ?? "队列设置已保存并生效");
}
async function persistDailyMomentsSettingsFromForm() {
  const current = runtime.dailyMomentSettings;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#daily-moments-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const number = (id: string) => Number(document.querySelector<HTMLInputElement>(id)?.value);
  const text = (id: string) => document.querySelector<HTMLInputElement>(id)?.value.trim() ?? "";
  const windowsText = document.querySelector<HTMLTextAreaElement>("#daily-moment-windows")?.value.trim() ?? "";
  let scheduleWindows: DailyMomentsSettings["scheduleWindows"] = null;
  if (windowsText) {
    try {
      const parsed: unknown = JSON.parse(windowsText);
      if (!Array.isArray(parsed)) throw new Error("必须是 JSON 数组");
      scheduleWindows = parsed as NonNullable<DailyMomentsSettings["scheduleWindows"]>;
    } catch (error) {
      const message = error instanceof Error ? error.message : "格式错误";
      const status = document.querySelector<HTMLElement>("#daily-moment-settings-status");
      if (status) status.textContent = `时间窗 JSON 无效：${message}`;
      notify("请检查随机时间窗 JSON");
      return;
    }
  }
  const settings: DailyMomentsSettings = {
    ...current,
    enabled: document.querySelector<HTMLInputElement>("#daily-moment-enabled")?.checked ?? current.enabled,
    hour: number("#daily-moment-hour"),
    minute: number("#daily-moment-minute"),
    intervalDays: number("#daily-moment-interval"),
    minMessagesPerGroup: number("#daily-moment-min-messages"),
    maxGroups: number("#daily-moment-max-groups"),
    maxMessagesPerGroup: number("#daily-moment-max-messages"),
    maxPromptChars: number("#daily-moment-max-prompt"),
    startupCatchup: document.querySelector<HTMLInputElement>("#daily-moment-startup-catchup")?.checked ?? false,
    allowImages: document.querySelector<HTMLInputElement>("#daily-moment-allow-images")?.checked ?? false,
    maxImages: number("#daily-moment-max-images"),
    visibility: number("#daily-moment-visibility"),
    targetUins: text("#daily-moment-target-uins").split(",").map((uin) => uin.trim()).filter(Boolean),
    maxResearchCalls: number("#daily-moment-research-calls"),
    maxRounds: number("#daily-moment-rounds"),
    scheduleWindows,
  };
  const error = await saveDailyMomentsSettings(settings, token);
  const status = document.querySelector<HTMLElement>("#daily-moment-settings-status");
  if (status) status.textContent = error ?? `配置已保存；Rust 草稿生成${settings.enabled ? "已启用" : "已停用"}，QQ 空间发布保持关闭`;
  notify(error ?? `每日动态配置已保存；生成${settings.enabled ? "已启用" : "已停用"}，不会自动发布`);
}
async function persistWebSearchSettingsFromForm() {
  const current = runtime.webSearchSettings;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#web-search-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const settings: WebSearchSettings = {
    enabled: document.querySelector<HTMLInputElement>("#web-search-enabled")?.checked ?? false,
    provider: current.provider,
    search_url: document.querySelector<HTMLInputElement>("#web-search-url")?.value.trim() ?? "",
    max_results: Number(document.querySelector<HTMLInputElement>("#web-search-max-results")?.value),
  };
  const error = await saveWebSearchSettings(settings, token);
  const status = document.querySelector<HTMLElement>("#web-search-settings-status");
  if (status) status.textContent = error ?? "已保存，新消息立即使用此设置";
  notify(error ?? "联网搜索设置已保存并生效");
}
async function persistAsrSettingsFromForm() {
  const current = runtime.asrSettings?.settings;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#asr-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const settings: AsrSettings = {
    enabled: document.querySelector<HTMLInputElement>("#asr-enabled")?.checked ?? false,
    provider: document.querySelector<HTMLSelectElement>("#asr-provider")?.value ?? current.provider,
    base_url: document.querySelector<HTMLInputElement>("#asr-base-url")?.value.trim() ?? "",
    model: document.querySelector<HTMLInputElement>("#asr-model")?.value.trim() ?? "",
    language: document.querySelector<HTMLInputElement>("#asr-language")?.value.trim() ?? "",
    max_per_hour: Number(document.querySelector<HTMLInputElement>("#asr-max-per-hour")?.value),
    timeout_ms: Number(document.querySelector<HTMLInputElement>("#asr-timeout")?.value) * 1000,
  };
  const apiKey = document.querySelector<HTMLInputElement>("#asr-api-key")?.value ?? "";
  const clearApiKey = document.querySelector<HTMLInputElement>("#asr-clear-api-key")?.checked ?? false;
  if (apiKey && clearApiKey) { notify("不能同时设置和清除 API 密钥"); return; }
  const error = await saveAsrSettings(settings, token, apiKey, clearApiKey);
  const status = document.querySelector<HTMLElement>("#asr-settings-status");
  if (status) status.textContent = error ?? "已保存，新消息立即使用此设置";
  notify(error ?? "语音识别设置已保存并生效");
}
async function persistTimeControlSettingsFromForm() {
  const current = runtime.timeControlSettings;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#time-control-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  let settings: TimeControlSettings;
  try {
    settings = readTimeForm(document);
  } catch (error) {
    const message = error instanceof Error ? error.message : "JSON 格式错误";
    const status = document.querySelector<HTMLElement>("#time-control-settings-status");
    if (status) status.textContent = `配置格式错误：${message}`;
    return;
  }
  const error = await saveTimeControlSettings(settings, token);
  if (!error) { delete $("#tab-panel").dataset.structureDirty; setTab(activeTab, false); }
  const status = document.querySelector<HTMLElement>("#time-control-settings-status");
  if (status) status.textContent = error ?? "已保存，新消息与待发送消息立即使用此时间表";
  notify(error ?? "时间控制设置已保存并生效");
}
async function persistBehaviorSettingsFromForm() {
  const token = document.querySelector<HTMLInputElement>("#behavior-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  let settings: BehaviorSettings;
  try {
    if (!runtime.behaviorSettings) return;
    settings = readBehaviorForm(document, runtime.behaviorSettings);
  } catch (error) {
    const message = error instanceof Error ? error.message : "JSON 格式错误";
    const status = document.querySelector<HTMLElement>("#behavior-settings-status");
    if (status) status.textContent = `配置格式错误：${message}`;
    return;
  }
  if (settings.access.allow_all_when_empty && !runtime.behaviorSettings?.access.allow_all_when_empty && !window.confirm("白名单为空时将允许全部会话进入回复流程。确认修改回复范围？")) return;
  const error = await saveBehaviorSettings(settings, token);
  const status = document.querySelector<HTMLElement>("#behavior-settings-status");
  if (status) status.textContent = error ?? "已保存，访问策略和触发规则立即生效";
  notify(error ?? "消息策略已保存并生效");
}
async function persistModelSettingsFromForm() {
  const current = runtime.modelSettings?.settings;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#model-admin-token")?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理令牌"); return; }
  const number = (id: string) => Number(document.querySelector<HTMLInputElement>(id)?.value);
  const settings: ModelSettings = {
    base_url: document.querySelector<HTMLInputElement>("#model-base-url")?.value.trim() ?? "",
    model: document.querySelector<HTMLInputElement>("#model-id")?.value.trim() ?? "",
    system_prompt: document.querySelector<HTMLTextAreaElement>("#model-system-prompt")?.value ?? "",
    temperature: number("#model-temperature"),
    max_tokens: number("#model-max-tokens"),
    max_tool_rounds: number("#model-max-tool-rounds"),
    timeout_ms: number("#model-timeout-ms"),
    fallback: {
      enabled: document.querySelector<HTMLInputElement>("#model-fallback-enabled")?.checked ?? false,
      base_url: document.querySelector<HTMLInputElement>("#model-fallback-base-url")?.value.trim() ?? "",
      model: document.querySelector<HTMLInputElement>("#model-fallback-id")?.value.trim() ?? "",
      timeout_ms: number("#model-fallback-timeout-ms"),
    },
    vision_model: {
      enabled: document.querySelector<HTMLInputElement>("#model-vision-enabled")?.checked ?? false,
      base_url: document.querySelector<HTMLInputElement>("#model-vision-base-url")?.value.trim() ?? "",
      model: document.querySelector<HTMLInputElement>("#model-vision-id")?.value.trim() ?? "",
      timeout_ms: number("#model-vision-timeout-ms"),
    },
  };
  const apiKey = document.querySelector<HTMLInputElement>("#model-api-key")?.value ?? "";
  const clearApiKey = document.querySelector<HTMLInputElement>("#model-clear-api-key")?.checked ?? false;
  const fallbackApiKey = document.querySelector<HTMLInputElement>("#model-fallback-api-key")?.value ?? "";
  const clearFallbackApiKey = document.querySelector<HTMLInputElement>("#model-fallback-clear-api-key")?.checked ?? false;
  const visionApiKey = document.querySelector<HTMLInputElement>("#model-vision-api-key")?.value ?? "";
  const clearVisionApiKey = document.querySelector<HTMLInputElement>("#model-vision-clear-api-key")?.checked ?? false;
  const result = await saveModelSettings(settings, token, apiKey, clearApiKey, fallbackApiKey, clearFallbackApiKey, visionApiKey, clearVisionApiKey);
  const status = document.querySelector<HTMLElement>("#model-settings-status");
  if (status) status.textContent = result?.error ?? "模型配置已保存并立即生效；密钥不会回显";
  const keyInput = document.querySelector<HTMLInputElement>("#model-api-key");
  if (keyInput) keyInput.value = "";
  const fallbackKeyInput = document.querySelector<HTMLInputElement>("#model-fallback-api-key");
  if (fallbackKeyInput) fallbackKeyInput.value = "";
  const visionKeyInput = document.querySelector<HTMLInputElement>("#model-vision-api-key");
  if (visionKeyInput) visionKeyInput.value = "";
  notify(result?.error ?? "模型设置已保存并生效");
}
function setTab(tab: string, sound = true) {
  tab = resolveTab(records[selected].title, tab).id;
  if (sound && tab === activeTab) return;
  activeTab = tab;
  if (sound && currentManagementItem()) {
    const url = new URL(location.href);
    url.searchParams.set("tab", tab);
    history.pushState({ managementRoute: url.pathname }, "", url.pathname + url.search);
  }
  document.querySelectorAll("[data-tab]").forEach((b) => {
    const active = (b as HTMLElement).dataset.tab === tab;
    b.classList.toggle("active", active);
    b.setAttribute("aria-selected", String(active));
    b.setAttribute("tabindex", active ? "0" : "-1");
  });
  const tabButton = $<HTMLButtonElement>(`[data-tab="${tab}"]`);
  const indicator = $(".tab-indicator");
  indicator.style.transition = sound && motionActive("surfaceTransitions") ? "" : "none";
  indicator.style.transform = `translateX(${tabButton.offsetLeft}px) scaleX(${tabButton.offsetWidth})`;
  $("#tab-panel").setAttribute("aria-labelledby", tabButton.id);
  delete $("#tab-panel").dataset.structureDirty;
  $("#tab-panel").innerHTML = workspaceMarkup(resolveTab(records[selected].title, tab).view);
  $("#tab-panel").scrollTop = 0;
  syncWorkspaceConnection();
  syncManagementNavigation();
  documentDecryption.refresh();
  if (sound) {
    tabTransition.reveal($("#tab-panel"), !motionActive("surfaceTransitions"));
    audio.play("ui-tick");
  }
}

function workspaceMarkup(view: string): string {
  if (view === "messages") return messageArchiveMarkup();
  if (view === "events") return eventsMarkup();
  if (view === "features") return featureModulesMarkup() + incidentLedgerMarkup("incident-lifecycle");
  if (view === "memory") return memoryConsolidationMarkup();
  if (view === "handoff") return memoryHandoffMarkup();
  if (view === "relationships" || view === "relationship-settings") return relationshipArchiveMarkup(view);
  if (view === "people" || view === "requests") return identityArchiveMarkup(view);
  if (view === "invites" || view === "invite-settings") return groupInviteArchiveMarkup(view);
  if (view === "qzone" || view === "qzone-settings") return qzoneInteractionsMarkup(view);
  if (view === "daily") return dailyMomentsArchiveMarkup();
  if (view === "wakes" || view === "proactive-settings") return proactiveMarkup(view);
  if (view === "stickers") return stickerArchiveMarkup();
  if (view === "unknown") return unknownOutboxMarkup();
  if (["incidents", "incident-settings", "chat-controls"].includes(view)) return incidentLedgerMarkup(view);
  if (view === "tasks") return runtimeMarkup("自动化档案", "任务档案");
  if (view === "asr") return runtimeMarkup("能力档案", "语音识别");
  if (view === "status" || view === "maintenance") {
    const snapshot = runtime.snapshot;
    if (!snapshot) return "<p>正在读取运行状态…</p>";
    const status = snapshot.status;
    return `<div class="incident-summary"><span>核心服务<b>${status.core_ready ? "正常" : "等待就绪"}</b></span><span>NapCat<b>${status.onebot_connected ? "已连接" : "未连接"}</b></span><span>运行模式<b>${runtime.config?.observe_only ? "观察模式" : "活动模式"}</b></span></div><div class="runtime-inline">
      <div class="runtime-row"><span>运行时长</span><b>${Math.floor(status.uptime_seconds / 3600)} 小时 ${Math.floor(status.uptime_seconds % 3600 / 60)} 分钟</b></div>
      <div class="runtime-row"><span>消息记录</span><b>${snapshot.counts.messages}</b></div><div class="runtime-row"><span>消息队列</span><b>${status.queue_depth} / ${status.queue_capacity}</b></div>
      <div class="runtime-row"><span>待人工核对外发</span><b>${snapshot.counts.unknown_outbox}</b></div>
      <div class="runtime-row"><span>数据库版本</span><b>${status.migration_version}</b></div>
      ${view === "maintenance" ? `<div class="runtime-row"><span>服务版本</span><b>${escapeHtml(runtime.health?.version ?? "未读取")}</b></div><div class="runtime-row"><span>数据目录</span><b>${escapeHtml(runtime.config?.data_dir ?? "未读取")}</b></div><small>更新、备份和切换由本机运维工具执行，此页不执行重启或数据恢复。</small>` : ""}</div>`;
  }
  return runtimeMarkup("系统档案", "配置档案", view);
}

function syncWorkspaceConnection() {
  const indicator = document.querySelector<HTMLElement>("#workspace-connection");
  if (indicator) indicator.textContent = runtime.connected ? "服务已连接" : "服务未连接";
  const notice = document.querySelector<HTMLElement>("#workspace-notice");
  const requiresLogin = runtime.apiErrors["/api/snapshot"]?.includes("401");
  const view = resolveTab(records[selected].title, activeTab).view;
  const viewRoutes: Record<string, string[]> = {
    messages: ["/api/messages"], tasks: ["/api/tasks"], unknown: ["/api/outbox/unknown"],
    models: ["/api/settings/model"], asr: ["/api/settings/asr"], "web-search": ["/api/settings/web-search"],
    behavior: ["/api/settings/behavior"], schedule: ["/api/settings/time-control"], "queue-settings": ["/api/settings/queue"],
    people: ["/api/identity/people"], requests: ["/api/identity/incoming-friend-requests"],
    invites: ["/api/group-invites"], "invite-settings": ["/api/group-invites/settings"],
    relationships: ["/api/relationships"], "relationship-settings": ["/api/relationship-pilot/settings"],
    incidents: ["/api/incidents"], "chat-controls": ["/api/incidents/chat-controls"], "incident-settings": ["/api/incident-pilot/settings"],
    qzone: ["/api/qzone-interactions/status"], "qzone-settings": ["/api/qzone-interactions/settings"],
    wakes: ["/api/proactive/status"], "proactive-settings": ["/api/proactive/status"], memory: ["/api/memory-consolidation/status"],
    features: ["/api/relationship-pilot/status", "/api/qzone-interactions/status", "/api/proactive/status", "/api/stickers/settings", "/api/sticker-collection/settings", "/api/memory-consolidation/status", "/api/multimodal-context/status"],
  };
  const moduleError = (viewRoutes[view] ?? []).map((path) => runtime.apiErrors[path]).find(Boolean);
  if (notice) {
    if (requiresLogin && !notice.querySelector("#session-token")) notice.innerHTML = `<label class="runtime-row"><span>管理令牌（仅保存在当前页面内存）</span><input id="session-token" type="password" autocomplete="off" /></label><button type="button" class="solid-button" data-action="connect-session">连接管理服务</button><small id="session-status" role="status"></small>`;
    else if (!requiresLogin) notice.textContent = !runtime.connected ? "服务未连接，保留最后一次数据；管理操作暂不可用。" : moduleError ? `本页数据读取失败：${moduleError}。保留上次数据，操作暂不可用。` : "";
  }
  document.querySelectorAll<HTMLButtonElement>("#tab-panel button[data-action]").forEach((button) => {
    if (!runtime.connected || moduleError) { if (button.dataset.offlineDisabled === undefined) button.dataset.offlineDisabled = String(button.disabled); button.disabled = true; }
    else if (button.dataset.offlineDisabled !== undefined) { button.disabled = button.dataset.offlineDisabled === "true"; delete button.dataset.offlineDisabled; }
  });
  if (getAdminToken()) document.querySelectorAll<HTMLInputElement>('#tab-panel input[type="password"]').forEach((input) => {
    if (/token$/.test(input.id)) input.closest("label")?.setAttribute("hidden", "");
  });
  let connection = document.querySelector<HTMLElement>("#rinbot-connection");
  if (!connection) {
    connection = document.createElement("div");
    connection.id = "rinbot-connection";
    connection.className = "rinbot-bridge";
    $(".brand").append(connection);
  }
  connection.textContent = runtime.connected ? `Rust 服务已连接 · NapCat ${runtime.snapshot?.status.onebot_connected ? "已连接" : "未连接"}` : "Rust 服务未连接";
}

function eventsMarkup() {
  return `<div class="runtime-inline"><div class="panel-label">本次连接的实时事件 · ${runtime.events.length} 条</div>${runtime.events.length ? runtime.events.slice(0, 50).map((event) => `<div class="runtime-row"><span>${new Date(event.at).toLocaleTimeString("zh-CN")}</span><b>${escapeHtml(event.label)}</b></div>`).join("") : "<p>暂无事件。连接建立后的状态变化会显示在这里。</p>"}</div>`;
}
function notify(message: string) {
  clearTimeout(toastTimer);
  $("#toast").textContent = message;
  $("#toast").classList.add("visible");
  toastTimer = setTimeout(() => $("#toast").classList.remove("visible"), 2600);
}
async function handleCancelQueuedTask(taskId: number) {
  if (!window.confirm(`确定取消尚未开始处理的消息任务 ${taskId} 吗？`)) return;
  const token = document.querySelector<HTMLInputElement>("#task-admin-token")?.value.trim() || getAdminToken();
  if (!token) {
    notify("请先输入管理令牌");
    return;
  }
  const error = await cancelQueuedTask(taskId, token);
  const status = document.querySelector<HTMLElement>("#task-action-status");
  if (status) status.textContent = error ?? `任务 ${taskId} 已取消`;
  if (error) notify(error);
}
async function handleReconcileUnknownOutbox(id: string, outcome: "sent" | "failed", button: HTMLButtonElement) {
  const resultText = outcome === "sent" ? "QQ 中已经确认送达" : "QQ 中已经确认未送达";
  if (!window.confirm(`请先核对 QQ 会话，确认${resultText}。继续只会记账，不会重发；此结果会解除该条 unknown 状态。`)) return;
  const tokenInput = document.querySelector<HTMLInputElement>("#unknown-outbox-admin-token");
  const token = tokenInput?.value.trim() || getAdminToken();
  if (!token) { notify("请先输入管理员令牌"); return; }
  button.disabled = true;
  const error = await reconcileUnknownOutbox(id, outcome, token);
  if (tokenInput) tokenInput.value = "";
  const status = document.querySelector<HTMLElement>("#unknown-outbox-status");
  if (status) status.textContent = error ?? "对账结果已记录；未执行外发或重试";
  if (error) {
    button.disabled = false;
    notify(error);
  } else {
    notify("对账结果已记录；未执行外发或重试");
  }
}
function exportSelectedArchive() {
  const r = records[selected];
  const text = [
    `RInBot ${r.id} · ${r.title}`,
    r.title,
    `功能分组：${r.category}`,
    `所属模块：${r.module}`,
    `数据来源：${r.dataSource}`,
    `当前状态：${r.status}`,
    "",
    r.abstract,
    "",
    ...r.findings.map((finding, index) => `${index + 1}. ${finding}`),
  ].join("\n");
  const url = URL.createObjectURL(new Blob([text], { type: "text/plain;charset=utf-8" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = `RINBOT-${r.id}.txt`;
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 0);
  notify("档案已导出");
}

function openModal(kind: NonNullable<typeof modal>) {
  if (!ready) return;
  if (!modal) {
    previousFocus = document.activeElement as HTMLElement;
    modalSiblings = [...$("#stage").children]
      .filter((node): node is HTMLElement => node instanceof HTMLElement && node.id !== "modal-root")
      .map((node) => ({ node, inert: node.inert }));
    modalSiblings.forEach(({ node }) => (node.inert = true));
  }
  modalClosing = false;
  modal = kind;
  searchQuery = "";
  filter = "全部档案";
  audio.play("page-open");
  renderModal();
}
function closeModal(afterClose?: () => void) {
  if (!modal) {
    afterClose?.();
    return;
  }
  if (modalClosing) return;
  modalClosing = true;
  audio.play("page-close");
  modalTransition!.hide(!motionActive("surfaceTransitions"), () => {
    modal = null;
    modalClosing = false;
    $("#modal-root").replaceChildren();
    modalTransition = undefined;
    modalSiblings.forEach(({ node, inert }) => (node.inert = inert));
    modalSiblings = [];
    $("#archive-ui").inert = mode !== "archive" || Boolean(workbench?.enabled);
    $("#detail-ui").inert = mode !== "detail";
    previousFocus?.focus({ preventScroll: true });
    afterClose?.();
  });
}
function renderModal() {
  if (!modal) return;
  modalTransition?.dispose();
  $("#modal-root").innerHTML =
    `<div class="modal-backdrop"><section class="terminal-modal ${modal === "settings" ? "settings-modal" : ""}" role="dialog" aria-modal="true" aria-label="${modal === "settings" ? "系统设置" : modal === "saved" ? "收藏档案" : "档案检索"}"><div class="modal-top"><span>RInBot / ${modal === "settings" ? "系统设置" : "档案目录"}</span><button data-action="close-modal" aria-label="关闭窗口">关闭 <span>×</span></button></div>${modal === "settings" ? settingsMarkup() : `<h2>${modal === "saved" ? "收藏档案" : "档案检索"}<small>${modal === "saved" ? "已收藏的功能档案" : "搜索功能名称、编号或模块"}</small></h2><div class="search-field"><span>⌕</span><input id="archive-search" type="search" autocomplete="off" placeholder="输入档案编号、名称或模块" aria-label="检索档案"/><span class="key">ESC</span></div><div class="category-filters">${categories.map((c, i) => `<button data-filter="${escapeHtml(c)}" class="${i === 0 ? "active" : ""}">${escapeHtml(c)}</button>`).join("")}</div><div class="result-header"><span>档案 / 功能</span><span>分组 / 模块</span><span>当前状态</span></div><div id="search-results" class="search-results"></div><div class="modal-bottom"><span id="result-count"></span><span>RInBot 功能目录</span></div>`}</section></div>`;
  const backdrop = $(".modal-backdrop");
  backdrop.hidden = true;
  modalTransition = new SurfaceTransition(backdrop, $(".terminal-modal"));
  modalTransition.show(!motionActive("surfaceTransitions"));
  if (modal === "settings") updateQualitySummary();
  if (modal !== "settings") {
    renderResults();
    requestAnimationFrame(() => {
      if (backdrop.isConnected && !modalClosing) $("#archive-search").focus();
    });
  } else
    requestAnimationFrame(() => {
      if (backdrop.isConnected && !modalClosing) $('[data-action="close-modal"]').focus();
    });
  $("#modal-root")
    .querySelector(".modal-backdrop")
    ?.addEventListener("click", (e) => {
      if (e.target === e.currentTarget) closeModal();
    });
}
function renderResults() {
  const results = records
    .map((r, i) => ({ r, i }))
    .filter(
      ({ r }) =>
        (modal !== "saved" || saved.has(r.key)) &&
        (filter === "全部档案" || r.category === filter) &&
        `${r.id} ${r.title} ${r.en} ${r.module} ${r.dataSource}`
          .toLowerCase()
          .includes(searchQuery.toLowerCase()),
    );
  $("#search-results").innerHTML = results.length
    ? results
        .map(
          ({ r, i }) =>
            `<button class="result-row" data-result="${i}"><span class="result-name"><b>${r.id}</b><span>${escapeHtml(r.title)}<small>${escapeHtml(r.module)}</small></span>${saved.has(r.key) ? "<i>＋</i>" : ""}</span><span>${escapeHtml(r.category)}</span><span>${escapeHtml(r.status)} <i>↗</i></span></button>`,
        )
        .join("")
    : `<div class="empty-results"><span>∅</span><strong>${modal === "saved" && !searchQuery ? "尚无收藏档案" : "没有匹配的档案"}</strong><p>${modal === "saved" && !searchQuery ? "打开档案后，可将常用项目加入收藏。" : "尝试其他名称、档案编号或所属模块。"}</p><button data-action="reset-search">${modal === "saved" ? "查看全部档案 →" : "清除检索条件 →"}</button></div>`;
  $("#result-count").textContent =
    `找到 ${String(results.length).padStart(2, "0")} 项`;
}
function updateQualitySummary() {
  const summary = document.querySelector("#quality-summary");
  if (!summary) return;
  if (!scene) { summary.textContent = "3D 已关闭 · 三维模型与渲染资源已释放"; return; }
  const canvas = scene.renderer.domElement;
  const metrics = JSON.parse(canvas.parentElement?.dataset.renderQuality ?? "{}");
  summary.textContent = `${superPerformanceEnabled() ? "超级性能模式已启用 · 画质设置暂被覆盖，关闭后恢复 · " : ""}实际渲染 ${canvas.width} × ${canvas.height} · ${effectiveRenderQuality().antialias === "smaa" ? "SMAA" : "原始抗锯齿"} · 纹理 ${metrics.anisotropy ?? 1}×${metrics.limited ? " · 已达到缓冲上限" : ""}`;
}
function motionPreferenceNoteMarkup() {
  const preset = prefs.motionPreset;
  const allEnabled = Object.values(prefs.motion).every(Boolean);
  return `<div id="motion-preference-note" class="motion-preference-note"><p>${motionSummary(prefs.motion)}</p><span>预设：${preset === "full" ? "完整动画" : preset === "reduced" ? "减少动画" : "自定义"} · 选择会保存在本站</span>${allEnabled ? "" : '<button data-action="enable-motion">启用完整动画并重播 ↻</button>'}</div>`;
}
function settingsMarkup() {
  const wallpaperNotice = isWallpaper
    ? '<p class="wallpaper-settings-note">每次启动都会读取 Wallpaper Engine 中的设置。在此修改仅对当前运行生效，无法持久保存；如需保留，请在 Wallpaper Engine 的壁纸属性中调整。</p>'
    : "";
  const performance = !isWallpaper
    ? `<label><div><strong>三维性能模式</strong><span>降低三维画质和渲染分辨率，保留完整动效；关闭后恢复原画质</span></div><input type="checkbox" data-pref="superPerformance" ${prefs.superPerformance ? "checked" : ""}/><i class="toggle"></i></label>`
    : "";
  const shortcuts = isWallpaper
    ? '<span>桌面操作</span><p>拖动阵列或点击界面按钮浏览档案。桌面模式下，方向键与滚轮可能无法传入壁纸。</p>'
    : '<span>键盘操作</span><p><kbd>←</kbd><kbd>→</kbd> 切换分组 <kbd>↑</kbd><kbd>↓</kbd> 选择档案 <kbd>ENTER</kbd> 查看 <kbd>/</kbd> 检索 <kbd>ESC</kbd> 返回</p>';
  return `<h2>系统设置<small>终端偏好设置</small></h2><p class="settings-intro">RInBot <span>·</span> 本地管理会话已就绪</p>${wallpaperNotice}<div class="settings-list">${themeSettingsMarkup(prefs.colorTheme === "dark")}${performance}${workbench?.settingsMarkup() ?? ""}${audioSettingsMarkup(prefs)}</div>${motionPreferenceNoteMarkup()}${motionSettingsMarkup(prefs.motion, prefs.motionPreset)}${qualityMarkup(prefs.rendering)}${pwaSettingsMarkup()}<div class="settings-shortcuts">${shortcuts}</div><div class="settings-bottom">${!isWallpaper && document.fullscreenEnabled ? '<button data-action="fullscreen">全屏显示 <span>↗</span></button>' : ""}<button data-action="restart">重播开场动画 <span>↻</span></button></div><div class="modal-bottom"><span>RInBot / 0.1 · 使用 MiSans 字体（小米） <a href="${assetUrl("fonts/MiSans-license.pdf")}" target="_blank" rel="noopener">字体许可</a></span><span>界面资源沿用 RhineLabUI</span></div>`;
}

document.addEventListener("input", (e) => {
  const slider = e.target as HTMLInputElement;
  if (slider.dataset.quality) {
    const output = document.querySelector<HTMLOutputElement>(`[data-quality-output="${slider.dataset.quality}"]`);
    if (output) output.value = `${slider.value}%`;
  }
  const volume = e.target as HTMLInputElement;
  if (volume.dataset.volume === "musicVolume" || volume.dataset.volume === "soundVolume") {
    prefs[volume.dataset.volume] = Number(volume.value) / 100;
    volume.closest("label")?.querySelector("output")?.replaceChildren(`${volume.value}%`);
    saveAudioPrefs();
  }
  if ((e.target as HTMLElement).id === "archive-search") {
    searchQuery = (e.target as HTMLInputElement).value;
    renderResults();
  }
});
document.addEventListener("change", (e) => {
  const el = e.target as HTMLInputElement;
  if (el.id === "quality-preset" && Object.hasOwn(qualityPresets, el.value)) {
    prefs.rendering = { ...qualityPresets[el.value as QualityPreset] };
    savePrefs();
  } else if (el.dataset.quality) {
    const key = el.dataset.quality as keyof RenderQuality;
    prefs.rendering = normalizeQuality({ ...prefs.rendering, [key]: key === "antialias" ? el.value : Number(el.value) });
    savePrefs();
  }
  if (el.dataset.pref) {
    const key = el.dataset.pref;
    if (key === "sound" || key === "music" || key === "quality" || key === "superPerformance") prefs[key] = el.checked;
    if (key === "sound" || key === "music") saveAudioPrefs(); else savePrefs();
    audio.play("confirm");
  }
  if (el.dataset.motion) {
    const key = el.dataset.motion as MotionKey;
    prefs.motion[key] = el.checked;
    prefs.motionPreset = motionPresetFor(prefs.motion);
    savePrefs();
    const motionRoot = $("#motion-settings");
    const advancedOpen = motionRoot.querySelector<HTMLDetailsElement>(".motion-advanced")?.open ?? false;
    const settingsPanel = motionRoot.closest<HTMLElement>(".settings-modal");
    const scrollTop = settingsPanel?.scrollTop ?? 0;
    motionRoot.outerHTML = motionSettingsMarkup(prefs.motion, prefs.motionPreset);
    $("#motion-preference-note").outerHTML = motionPreferenceNoteMarkup();
    $("#motion-settings").querySelector<HTMLDetailsElement>(".motion-advanced")!.open = advancedOpen;
    requestAnimationFrame(() => {
      if (settingsPanel) settingsPanel.scrollTop = scrollTop;
      document.querySelector<HTMLInputElement>(`[data-motion="${key}"]`)?.focus({ preventScroll: true });
    });
    notify(key === "boot" ? "开场设置将在下次重播时生效" : el.checked ? "已启用此动画" : "已关闭此动画");
    audio.play("confirm");
  }
});
document.addEventListener("click", (e) => {
  const themeButton = (e.target as Element).closest<HTMLElement>("[data-color-theme]");
  if (themeButton) { prefs.colorTheme = themeButton.dataset.colorTheme === "dark" ? "dark" : "light"; savePrefs(); return; }
  if (!started) return;
  if (modalClosing) return;
  const el = (e.target as Element).closest<HTMLElement>("button");
  if (!el) return;
  if (el.dataset.action === "motion-preset") {
    const preset = el.dataset.preset;
    if (preset !== "full" && preset !== "reduced") return;
    prefs.motionPreset = preset;
    prefs.motion = preset === "full" ? fullMotion() : reducedMotion();
    savePrefs();
    renderModal();
    requestAnimationFrame(() => document.querySelector<HTMLButtonElement>(`[data-action="motion-preset"][data-preset="${prefs.motionPreset}"]`)?.focus({ preventScroll: true }));
    audio.play("confirm");
    return;
  }
  if (el.dataset.select) {
    select(Number(el.dataset.select));
    return;
  }
  if (el.dataset.result) {
    const index = Number(el.dataset.result);
    closeModal(() => {
      select(index);
      openFile();
    });
    return;
  }
  if (el.dataset.filter) {
    filter = el.dataset.filter;
    document
      .querySelectorAll("[data-filter]")
      .forEach((b) =>
        b.classList.toggle(
          "active",
          (b as HTMLElement).dataset.filter === filter,
        ),
      );
    renderResults();
    return;
  }
  if (el.dataset.tab) {
    setTab(el.dataset.tab);
    return;
  }
  const action = el.dataset.action;
  if (action === "add-time-window") { $("#time-windows").insertAdjacentHTML("beforeend", timeWindowMarkup()); $("#tab-panel").dataset.structureDirty = "true"; return; }
  if (action === "remove-time-window") { el.closest(".time-window")?.remove(); $("#tab-panel").dataset.structureDirty = "true"; return; }
  if (action === "connect-session") {
    const token = document.querySelector<HTMLInputElement>("#session-token")?.value || getAdminToken();
    void authenticateSession(token).then(async (ok) => {
      if (ok) { await refreshRuntime(); notify("管理服务已连接；令牌不会写入本地存储"); }
      else { const status = document.querySelector("#session-status"); if (status) status.textContent = "连接失败，请检查令牌和服务状态。"; }
    });
    return;
  }
  if (action === "refresh-workspace") { void refreshRuntime(); return; }
  if (action === "filter-messages") {
    messageQuery = document.querySelector<HTMLInputElement>("#message-query")?.value.trim() ?? "";
    if (currentManagementItem()) {
      const url = new URL(location.href);
      if (messageQuery) url.searchParams.set("q", messageQuery); else url.searchParams.delete("q");
      history.replaceState(null, "", url.pathname + url.search);
    }
    setTab(activeTab, false);
    return;
  }
  if (action === "archive-page") {
    if (el.dataset.kind === "messages" || el.dataset.kind === "tasks") void loadArchivePage(el.dataset.kind, Number(el.dataset.offset));
    return;
  }
  if (el.closest("#tab-panel") && action && !runtime.connected) { notify("服务未连接，暂不能执行管理操作"); return; }
  if (action === "memory-handoff-load") {
    const token = document.querySelector<HTMLInputElement>("#memory-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadMemoryHandoffs(token);
    return;
  }
  if (action === "sticker-archive-load") {
    const token = document.querySelector<HTMLInputElement>("#sticker-admin-token")?.value.trim() || getAdminToken();
    const query = document.querySelector<HTMLInputElement>("#sticker-search")?.value.trim() ?? "";
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadStickerArchive(token, query).then(() => {
      const field = document.querySelector<HTMLInputElement>("#sticker-admin-token");
      if (field) field.value = "";
      notify(runtime.stickerArchiveError || "贴纸档案已载入；此页不会执行 QQ 操作");
    });
    return;
  }
  if (action === "sticker-create") {
    const token = document.querySelector<HTMLInputElement>("#sticker-admin-token")?.value.trim() || getAdminToken();
    const file = document.querySelector<HTMLInputElement>("#sticker-image-file")?.files?.[0];
    const imageUrl = document.querySelector<HTMLInputElement>("#sticker-image-url")?.value.trim() ?? "";
    if (!token) { notify("请先输入管理令牌"); return; }
    if ((file ? 1 : 0) + (imageUrl ? 1 : 0) !== 1) { notify("请选择一个图片文件，或填写一个图片网址"); return; }
    const readFile = (selected: File) => new Promise<string>((resolve, reject) => {
      const reader = new FileReader();
      reader.onerror = () => reject(new Error("读取图片文件失败"));
      reader.onload = () => typeof reader.result === "string" ? resolve(reader.result) : reject(new Error("图片格式读取失败"));
      reader.readAsDataURL(selected);
    });
    const tags = (document.querySelector<HTMLTextAreaElement>("#sticker-create-tags")?.value ?? "").split(/[\n,，]/).map((tag) => tag.trim()).filter(Boolean);
    const fields = {
      desc: document.querySelector<HTMLInputElement>("#sticker-create-desc")?.value.trim() ?? "",
      localNote: document.querySelector<HTMLInputElement>("#sticker-create-note")?.value.trim() ?? "",
      tags,
      usage: document.querySelector<HTMLInputElement>("#sticker-create-usage")?.value.trim() ?? "",
    };
    void (async () => {
      try {
        const imageDataUrl = file ? await readFile(file) : "";
        const error = await createStickerEntry(token, imageDataUrl, fields, imageUrl);
        notify(error || "本地贴纸已新增");
        if (!error) {
          const input = document.querySelector<HTMLInputElement>("#sticker-image-file"); if (input) input.value = "";
          const url = document.querySelector<HTMLInputElement>("#sticker-image-url"); if (url) url.value = "";
          for (const selector of ["#sticker-create-desc", "#sticker-create-note", "#sticker-create-tags", "#sticker-create-usage"]) {
            const field = document.querySelector<HTMLInputElement | HTMLTextAreaElement>(selector); if (field) field.value = "";
          }
        }
      } catch (error) { notify(error instanceof Error ? error.message : "新增贴纸失败"); }
    })();
    return;
  }
  if (["sticker-save", "sticker-delete", "sticker-preview"].includes(action ?? "")) {
    const token = document.querySelector<HTMLInputElement>("#sticker-admin-token")?.value.trim() || getAdminToken();
    const card = el.closest<HTMLElement>("[data-sticker-id]");
    const stickerId = card?.dataset.stickerId ?? "";
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!card || !stickerId) return;
    if (action === "sticker-preview") {
      void loadStickerPreview(token, stickerId).catch((error) => notify(error instanceof Error ? error.message : "图片读取失败"));
      return;
    }
    if (action === "sticker-save") {
      const read = (name: string) => card.querySelector<HTMLInputElement | HTMLTextAreaElement>(`[data-sticker-field="${name}"]`)?.value.trim() ?? "";
      const fields = { desc: read("desc"), localNote: read("localNote"), tags: read("tags").split(/[\n,，]/).map((tag) => tag.trim()).filter(Boolean), usage: read("usage") };
      void updateStickerEntry(token, stickerId, fields)
        .then((error) => notify(error || "贴纸资料已保存"))
        .catch((error) => notify(error instanceof Error ? error.message : "保存贴纸资料失败"));
      return;
    }
    if (!window.confirm("确认移除此贴纸记录？QQ 收藏只会在本地隐藏，不会调用 QQ 删除；本地贴纸将删除档案并尝试清理图片文件。")) return;
    void deleteStickerEntry(token, stickerId)
      .then((message) => notify(message || "贴纸记录已处理"))
      .catch((error) => notify(error instanceof Error ? error.message : "贴纸记录处理失败"));
    return;
  }
  if (action === "sticker-qq-sync") {
    const token = document.querySelector<HTMLInputElement>("#sticker-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!runtime.stickerLifecycle?.runtimeReady) { notify("贴纸模块尚未通过运行验收，当前不能同步"); return; }
    if (!window.confirm("从 NapCat 读取完整 QQ 收藏列表并合并本地贴纸档案？已有备注、标签和使用记录会保留。")) return;
    void syncStickerFavorites(token).finally(() => {
      const field = document.querySelector<HTMLInputElement>("#sticker-admin-token");
      if (field) field.value = "";
    });
    return;
  }
  if (action === "qzone-audit-load") {
    const token = document.querySelector<HTMLInputElement>("#qzi-audit-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadQzoneInteractionsAudit(token);
    return;
  }
  if (action === "qzone-manual-run") {
    const token = document.querySelector<HTMLInputElement>("#qzi-run-token")?.value.trim() || getAdminToken();
    const kind = document.querySelector<HTMLSelectElement>("#qzi-manual-kind")?.value ?? "all";
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!runtime.qzoneInteractionsStatus?.runtimeReady || !runtime.qzoneInteractionsSettings?.enabled) {
      notify("空间互动运行门槛未通过或功能已停用"); return;
    }
    const kindLabel = kind === "feed" ? "仅读取并处理好友动态" : kind === "reply" ? "仅读取并处理评论回复" : "读取并处理好友动态与评论回复";
    if (!window.confirm(`确认${kindLabel}？若计划包含点赞、评论或回复，将按当前参数尝试一次；结果不明的外部写入会隔离为 unknown，不自动重试。`)) return;
    void runQzoneInteractions(kind as "all" | "feed" | "reply", token).then((message) => notify(message || "手动运行已受理")).finally(() => {
      const field = document.querySelector<HTMLInputElement>("#qzi-run-token");
      if (field) field.value = "";
    });
    return;
  }
  if (action === "daily-moments-archive-load") {
    const token = document.querySelector<HTMLInputElement>("#daily-moments-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadDailyMomentArchive(token);
    return;
  }
  if (action === "daily-moment-run-preview") {
    const token = document.querySelector<HTMLInputElement>("#daily-moments-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!runtime.dailyMomentGenerationAvailable) { notify("每日动态生成运行链尚未就绪"); return; }
    void runDailyMomentPreview(token).then((error) => {
      const field = document.querySelector<HTMLInputElement>("#daily-moments-admin-token");
      if (field) field.value = "";
      notify(error ?? runtime.dailyMomentRunStatus);
    });
    return;
  }
  if (action === "daily-moment-publish" && el.dataset.id) {
    const token = document.querySelector<HTMLInputElement>("#daily-moments-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!runtime.dailyMomentRuntimeReady) { notify("每日动态发布链尚未通过运行验收"); return; }
    if (!window.confirm("确认将此草稿发布到 QQ 空间？确认后只尝试一次；若结果未知，系统不会自动重试。")) return;
    void publishDailyMomentDraft(el.dataset.id, token).then((error) => {
      notify(error ?? "发布操作已记录；如结果未知，请在档案中人工核对");
    });
    return;
  }
  if ((action === "daily-moment-resolve-sent" || action === "daily-moment-resolve-missed") && el.dataset.id) {
    const token = document.querySelector<HTMLInputElement>("#daily-moments-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    const outcome = action === "daily-moment-resolve-sent" ? "sent" : "missed";
    if (!window.confirm(outcome === "sent"
      ? "请先在 QQ 空间确认该动态确已发出。继续只会记账，不会再次发布。"
      : "请先确认 QQ 空间中没有这条动态。继续将解除待核对状态，不会触发发布。")) return;
    const tid = outcome === "sent" ? window.prompt("输入 QQ 空间动态 ID（tid），用于审计")?.trim() : undefined;
    if (outcome === "sent" && !tid) { notify("确认已发布必须填写动态 ID"); return; }
    const note = window.prompt("填写核对依据（例如空间链接或检查时间）")?.trim();
    if (!note) { notify("必须填写核对依据"); return; }
    void resolveDailyMomentPublication(el.dataset.id, outcome, token, note, tid)
      .then((error) => notify(error ?? "发布结果已记录；未执行发布或重试"));
    return;
  }
  if (action === "memory-handoff-clear" && el.dataset.chatKey) {
    const token = document.querySelector<HTMLInputElement>("#memory-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!window.confirm(`确认清除 ${el.dataset.chatKey} 的会话交接？此操作不可撤销。`)) return;
    void clearMemoryHandoff(el.dataset.chatKey, token).then((error) => notify(error ?? "会话交接已清除"));
    return;
  }
  if (action === "memory-handoff-save") {
    const card = el.closest<HTMLElement>("[data-handoff-card]");
    const chatKey = card?.dataset.chatKey;
    if (!card || !chatKey) return;
    const token = document.querySelector<HTMLInputElement>("#memory-admin-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    const field = (name: string) => card.querySelector<HTMLInputElement | HTMLTextAreaElement>(`[data-handoff-field="${name}"]`)?.value ?? "";
    const lines = (name: string) => field(name).split(/\r?\n/).map((value) => value.trim()).filter(Boolean);
    const ttlMinutes = Number(field("ttlMinutes"));
    if (!Number.isFinite(ttlMinutes) || ttlMinutes < 5 || ttlMinutes > 10_080) { notify("有效时间须在 5 到 10080 分钟之间"); return; }
    el.setAttribute("disabled", "true");
    void saveMemoryHandoff(chatKey, Number(card.dataset.updatedAt) || 0, {
      topic: field("topic").trim(), summary: field("summary").trim(),
      hypotheses: lines("hypotheses"), evidence: lines("evidence"), facts: lines("facts"),
      decisions: lines("decisions"), rejectedDirections: lines("rejectedDirections"),
      openQuestions: lines("openQuestions"), nextStep: field("nextStep").trim(), ttlMinutes,
    }, token).then((error) => notify(error ?? "会话交接已保存"))
      .finally(() => el.removeAttribute("disabled"));
    return;
  }
  if (action === "save-queue-settings") { void persistQueueSettingsFromForm(); return; }
  if (action === "save-daily-moment-settings") { void persistDailyMomentsSettingsFromForm(); return; }
  if (action === "save-web-search-settings") { void persistWebSearchSettingsFromForm(); return; }
  if (action === "save-asr-settings") { void persistAsrSettingsFromForm(); return; }
  if (action === "save-relationship-settings") { void persistRelationshipSettingsFromForm(); return; }
  if (action === "relationship-lifecycle-toggle") { void updateRelationshipLifecycle("toggle"); return; }
  if (action === "relationship-graduate") { void updateRelationshipLifecycle("graduate"); return; }
  if (action === "qzone-lifecycle-toggle") { void updateQzoneLifecycle("toggle"); return; }
  if (action === "qzone-graduate") { void updateQzoneLifecycle("graduate"); return; }
  if (action === "sticker-lifecycle-toggle") { void updateStickerLifecycle("toggle"); return; }
  if (action === "sticker-preferences-save") { void updateStickerPreferences(); return; }
  if (action === "sticker-graduate") { void updateStickerLifecycle("graduate"); return; }
  if (action === "sticker-collection-lifecycle-toggle") { void updateStickerCollectionLifecycle("toggle"); return; }
  if (action === "sticker-collection-graduate") { void updateStickerCollectionLifecycle("graduate"); return; }
  if (action === "proactive-lifecycle-toggle") { void updateProactiveFeatureLifecycle("toggle"); return; }
  if (action === "proactive-graduate") { void updateProactiveFeatureLifecycle("graduate"); return; }
  if (action === "memory-consolidation-lifecycle-toggle") { void updateMemoryConsolidationFeatureLifecycle("toggle"); return; }
  if (action === "memory-consolidation-graduate") { void updateMemoryConsolidationFeatureLifecycle("graduate"); return; }
  if (action === "multimodal-context-lifecycle-toggle") { void updateMultimodalContextFeatureLifecycle("toggle"); return; }
  if (action === "multimodal-context-graduate") { void updateMultimodalContextFeatureLifecycle("graduate"); return; }
  if (action === "proactive-load") {
    const token = document.querySelector<HTMLInputElement>("#proactive-load-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadProactiveData(token).then((error) => {
      const field = document.querySelector<HTMLInputElement>("#proactive-load-token");
      if (field) field.value = "";
      notify(error ?? "主动消息数据已载入");
    });
    return;
  }
  if (action === "save-proactive-settings") { void persistProactiveSettingsFromForm(); return; }
  if (action === "proactive-wake-cancel" && el.dataset.id) {
    const token = document.querySelector<HTMLInputElement>("#proactive-cancel-token")?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!window.confirm("确认取消这条待执行唤醒？已领取或已开始外发的任务不可在此取消。")) return;
    void cancelProactiveWake(el.dataset.id, token).then((error) => {
      const field = document.querySelector<HTMLInputElement>("#proactive-cancel-token");
      if (field) field.value = "";
      notify(error ?? "待执行唤醒已取消");
    });
    return;
  }
  if (action === "save-qzone-settings") { void persistQzoneSettingsFromForm(); return; }
  if (action === "save-time-control-settings") { void persistTimeControlSettingsFromForm(); return; }
  if (action === "save-behavior-settings") { void persistBehaviorSettingsFromForm(); return; }
  if (action === "save-model-settings") { void persistModelSettingsFromForm(); return; }
  if (action === "cancel-queued-task") {
    const taskId = Number(el.dataset.taskId);
    if (Number.isSafeInteger(taskId) && taskId > 0) void handleCancelQueuedTask(taskId);
    return;
  }
  if (action === "reconcile-unknown-outbox" && el.dataset.id && (el.dataset.outcome === "sent" || el.dataset.outcome === "failed")) {
    void handleReconcileUnknownOutbox(el.dataset.id, el.dataset.outcome, el as HTMLButtonElement);
    return;
  }
  if (action === "memory-consolidation-history") {
    const chatKey = document.querySelector<HTMLInputElement>("#memory-consolidation-chat")?.value.trim() ?? "";
    const userId = document.querySelector<HTMLInputElement>("#memory-consolidation-user")?.value.trim() ?? "";
    const tokenInput = document.querySelector<HTMLInputElement>("#memory-consolidation-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!/^(group|private):\d{1,15}$/.test(chatKey) || !/^\d{1,15}$/.test(userId)) { notify("请填写有效会话和 QQ 号"); return; }
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadMemoryConsolidationHistory(chatKey, userId, token).then((error) => {
      if (tokenInput) tokenInput.value = "";
      notify(error ?? "版本历史已载入");
    });
    return;
  }
  if (action === "model-memory-history") {
    const chatKey = document.querySelector<HTMLInputElement>("#memory-consolidation-chat")?.value.trim() ?? "";
    const userId = document.querySelector<HTMLInputElement>("#memory-consolidation-user")?.value.trim() ?? "";
    const tokenInput = document.querySelector<HTMLInputElement>("#memory-consolidation-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!/^(group|private):\d{1,15}$/.test(chatKey) || (userId && !/^\d{1,15}$/.test(userId))) { notify("请填写有效会话和 QQ 号（QQ 号可留空以查看全部）"); return; }
    if (!token) { notify("请先输入管理令牌"); return; }
    void loadModelMemoryMutationHistory(chatKey, userId, token).then((error) => {
      if (tokenInput) tokenInput.value = "";
      notify(error ?? "模型记忆变更记录已载入");
    });
    return;
  }
  if (action === "model-memory-restore") {
    const history = runtime.modelMemoryMutationHistory;
    const operationId = el.dataset.operationId ?? "";
    const tokenInput = document.querySelector<HTMLInputElement>("#memory-consolidation-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!history || !operationId) { notify("变更记录已失效，请重新读取"); return; }
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!window.confirm(`确认恢复操作 ${operationId} 删除的人物记忆？恢复会保留原删除记录，并新增一条管理员审计记录。`)) return;
    void restoreModelMemoryRemoval(operationId, history.chatKey, history.userId, token).then((error) => {
      if (tokenInput) tokenInput.value = "";
      notify(error ?? "已恢复人物记忆并记录审计");
    });
    return;
  }
  if (action === "memory-consolidation-restore") {
    const history = runtime.memoryConsolidationHistory;
    const targetRevision = Number(el.dataset.revision);
    const tokenInput = document.querySelector<HTMLInputElement>("#memory-consolidation-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!history || !Number.isSafeInteger(targetRevision) || targetRevision < 0) { notify("版本历史已失效，请重新读取"); return; }
    if (!token) { notify("请先输入管理令牌"); return; }
    const targetLabel = targetRevision === 0 ? "Node 导入前快照" : `版本 #${targetRevision}`;
    if (!window.confirm(`确认将 QQ ${history.userId} 的跨会话全局人物印象及来源恢复到${targetLabel}？这不只影响当前会话 ${history.chatKey}。操作会追加新版本，不删除历史；功能必须保持停用。`)) return;
    void restoreMemoryConsolidationRevision(history, targetRevision, token).then((error) => {
      if (tokenInput) tokenInput.value = "";
      notify(error ?? "已恢复并追加新版本");
    });
    return;
  }
  if (action === "memory-consolidation-node-backup-restore") {
    const history = runtime.memoryConsolidationHistory;
    const backupKey = el.dataset.backupKey ?? "";
    const tokenInput = document.querySelector<HTMLInputElement>("#memory-consolidation-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!history || !backupKey) { notify("Node 快照记录已失效，请重新读取历史"); return; }
    if (!token) { notify("请先输入管理令牌"); return; }
    if (!window.confirm(`确认将 QQ ${history.userId} 的全局人物印象恢复到此 Node 快照？这会影响跨会话来源，不只影响 ${history.chatKey}；系统将追加新版本并保留当前状态与全部历史。功能必须保持停用。`)) return;
    void restoreNodeMemoryConsolidationBackup(history, backupKey, token).then((error) => {
      if (tokenInput) tokenInput.value = "";
      notify(error ?? "Node 快照已恢复，并已追加全局版本");
    });
    return;
  }
  if (action === "memory-consolidation-run" || action === "memory-consolidation-run-all") {
    const chatKey = document.querySelector<HTMLInputElement>("#memory-consolidation-chat")?.value.trim() ?? "";
    const userIds = (document.querySelector<HTMLInputElement>("#memory-consolidation-user")?.value ?? "")
      .split(/[,，、\s]+/)
      .map((value) => value.trim())
      .filter(Boolean);
    const runAll = action === "memory-consolidation-run-all" || userIds.length === 0;
    const tokenInput = document.querySelector<HTMLInputElement>("#memory-consolidation-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!runtime.memoryConsolidationStatus?.manualReady) { notify("记忆整理尚未通过运行验收，未调用模型、未修改记忆"); return; }
    if (!/^(group|private):\d{1,15}$/.test(chatKey) || (!runAll && userIds.some((userId) => !/^\d{1,15}$/.test(userId)))) { notify("请填写有效会话和 QQ 号"); return; }
    if (!token) { notify("请先输入管理令牌"); return; }
    const targetDescription = runAll ? "该会话人物索引中的全部人物" : `${userIds.length} 位指定人物`;
    if (!window.confirm(`确认整理 ${chatKey} 中${targetDescription}的记忆？每人分别读取本人近期消息并追加版本；部分失败不会撤销已保存的其他人物版本。`)) return;
    void runMemoryConsolidation(chatKey, runAll ? null : userIds, token).then((error) => {
      if (tokenInput) tokenInput.value = "";
      notify(error ?? (runAll ? "已开始整理当前会话全部人物；结果会逐项显示" : `已处理 ${userIds.length} 位人物；结果已显示，旧版仍可追溯`));
    });
    return;
  }
  if (action === "identity-request-decision") {
    if ((el.dataset.decision === "approve" || el.dataset.decision === "reject") && el.dataset.id) {
      void handleIdentityRequestDecision(el.dataset.id, el.dataset.decision);
    }
    return;
  }
  if (action === "identity-whitelist-retry" && el.dataset.id) {
    void handleIdentityWhitelistRetry(el.dataset.id);
    return;
  }
  if (action === "identity-notification-retry" && el.dataset.id) {
    void handleIdentityNotificationRetry(el.dataset.id);
    return;
  }
  if (action === "group-invite-notification-retry" && el.dataset.id) {
    void handleGroupInviteNotificationRetry(el.dataset.id);
    return;
  }
  if (action === "group-invite-direct-notification-retry" && el.dataset.id) {
    void handleGroupInviteNotificationRetry(el.dataset.id, true);
    return;
  }
  if (action === "save-group-invite-settings") {
    void handleGroupInviteSettingsSave();
    return;
  }
  if (action === "identity-profile-edit" && el.dataset.uin) {
    void handleIdentityProfileEdit(el.dataset.uin);
    return;
  }
  if (action === "identity-person-merge" && el.dataset.uin) {
    void handleIdentityPersonMerge(el.dataset.uin);
    return;
  }
  if (action === "relationship-detail" && el.dataset.uin) {
    void loadRelationshipDetail(el.dataset.uin).then(() => setTab(activeTab, false));
    return;
  }
  if (action === "relationship-evaluation-trace" && el.dataset.uin && el.dataset.evaluationId) {
    const tokenInput = document.querySelector<HTMLInputElement>("#relationship-admin-token");
    const token = tokenInput?.value.trim() || getAdminToken();
    if (!token) { notify("请先输入管理员令牌"); return; }
    void loadRelationshipEvaluationTrace(el.dataset.uin, el.dataset.evaluationId, token)
      .then(() => setTab(activeTab, false));
    if (tokenInput) tokenInput.value = "";
    return;
  }
  if (action === "incident-acknowledge" && el.dataset.id) { void handleIncidentAction(el.dataset.id, "acknowledge"); return; }
  if (action === "incident-resolve" && el.dataset.id) { void handleIncidentAction(el.dataset.id, "resolve"); return; }
  if (action === "incident-delete" && el.dataset.id) { void handleIncidentAction(el.dataset.id, "delete"); return; }
  if (action === "incident-notification-retry" && el.dataset.id) { void handleIncidentAction(el.dataset.id, "notification/retry"); return; }
  if (action === "incident-lifecycle-toggle") { void handleIncidentLifecycle(); return; }
  if (action === "incident-graduate") { void handleIncidentLifecycle(true); return; }
  if (action === "incident-settings-save") { void handleIncidentOperationalSettings(el); return; }
  if (action === "save-chat-control") { void handleChatControlSave(el); return; }
  if (action === "toggle-three") { void toggleThree(); return; }
  if (action === "sound-preview") audio.play("confirm");
  if (action === "skip") {
    setMode("archive");
    audio.play("confirm");
  }
  if (action === "prev") stepFile(-1);
  if (action === "next") stepFile(1);
  if (action === "column-prev") stepColumn(-1);
  if (action === "column-next") stepColumn(1);
  if (action === "open") openFile();
  if (action === "back") {
    setMode("archive");
    audio.play("back");
  }
  if (action === "search" || action === "saved" || action === "settings") {
    el.focus({ preventScroll: true });
    openModal(action);
  }
  if (action === "close-modal") closeModal();
  if (action === "bookmark") toggleSaved();
  if (action === "export") exportSelectedArchive();
  if (action === "reset-search") {
    modal = "search";
    searchQuery = "";
    filter = "全部档案";
    renderModal();
  }
  if (action === "replay" || action === "restart") {
    replayBoot();
  }
  if (action === "enable-motion") {
    prefs.motion = fullMotion();
    prefs.motionPreset = "full";
    savePrefs();
    replayBoot();
  }
  if (action === "fullscreen" && document.fullscreenEnabled) {
    if (document.fullscreenElement) void document.exitFullscreen();
    else
      void document.documentElement
        .requestFullscreen()
        .catch(() => notify("请使用浏览器的全屏快捷键 F11"));
  }
});
document.addEventListener("keydown", (e) => {
  if (!started) return;
  if (playground?.active && !modal) {
    if (e.key === "Escape") { e.preventDefault(); playground.stop(); }
    else if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Enter", "/"].includes(e.key) && !(e.target instanceof HTMLButtonElement)) e.preventDefault();
    return;
  }
  if (modalClosing) {
    e.preventDefault();
    return;
  }
  const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLSelectElement || (e.target instanceof HTMLElement && e.target.isContentEditable);
  if (e.key === "Escape") {
    if (modal) closeModal();
    else if (mode === "detail" || (mode === "boot" && ready)) { const sound = mode === "detail" ? "back" : "ui-tick"; setMode("archive"); audio.play(sound); }
    return;
  }
  if (modal && e.key === "Tab") {
    const focusables = [
      ...$("#modal-root").querySelectorAll<HTMLElement>(
        'button,input:not(:disabled),select:not(:disabled),summary,[tabindex="0"]',
      ),
    ];
    const visible = focusables.filter(el => el.getClientRects().length > 0);
    const first = visible[0],
      last = visible.at(-1);
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last?.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first?.focus();
    }
    return;
  }
  if (typing || modal || !ready) return;
  if (
    (e.target as HTMLElement).dataset.tab &&
    ["ArrowLeft", "ArrowRight"].includes(e.key)
  ) {
    e.preventDefault();
    const tabs = pageFor(records[selected].title).tabs.map((tab) => tab.id);
    setTab(
      tabs[(tabs.indexOf(activeTab) + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length],
    );
    $<HTMLButtonElement>(`[data-tab="${activeTab}"]`).focus();
    return;
  }
  if (e.key === "/") {
    e.preventDefault();
    if (mode === "boot") setMode("archive");
    openModal("search");
  }
  if (e.key === "ArrowLeft" && mode !== "boot") {
    e.preventDefault();
    stepColumn(-1);
  }
  if (e.key === "ArrowRight" && mode !== "boot") {
    e.preventDefault();
    stepColumn(1);
  }
  if (["ArrowUp", "ArrowDown"].includes(e.key) && mode !== "boot") {
    e.preventDefault();
    stepFile(e.key === "ArrowUp" ? -1 : 1);
  }
  if (
    e.key === "Enter" &&
    (document.activeElement === document.body ||
      document.activeElement?.id === "detail-content" ||
      ["prev", "next", "column-prev", "column-next"].includes(
        (document.activeElement as HTMLElement)?.dataset.action ?? "",
      ) ||
      (document.activeElement as HTMLElement)?.dataset.select)
  ) {
    e.preventDefault();
    if (mode === "boot") setMode("archive");
    else if (mode === "archive") openFile();
  }
});

const ease = (t: number) => {
  t = Math.max(0, Math.min(1, t));
  return t * t * (3 - 2 * t);
};
function bootFrame(t: number) {
  if (isWallpaper && !scene && frozenTime === null && t >= 21.9) {
    setMode("archive");
    return undefined;
  }
  if (isWallpaper && frozenTime === null && t >= ARRAY_OPENING_END &&
      !openingShowsDetail(wallpaperHost()?.properties.openingdetail?.value, !!workbench?.enabled)) {
    setMode("archive");
    return undefined;
  }
  audio.updateBoot(t, frozenTime !== null);
  const motion = bootSequence.update(t);
  if (workbench?.enabled && frozenTime === null) {
    const end = openingShowsDetail(wallpaperHost()?.properties.openingdetail?.value, true) ? 35 : ARRAY_OPENING_END;
    if (t > end - .35) $(".powered").style.opacity = String(1 - ease((t - end + .35) / .35));
  }
  let step: string = motion.step;
  if (t >= 22) {
    step = "array";
  }
  if (t >= 25.68) {
    step = "select";
  }
  if (t >= 28.3) {
    step = "inspect";
  }
  if (step !== lastStep) {
    $("#stage").dataset.boot = step;
    lastStep = step;
  }
  $(".file-title").firstChild!.textContent =
    step === "array"
      ? "SELECTING FILES...".slice(0, Math.max(0, Math.floor((t - 21.94) * 18)))
      : "FILE NUMBER: ";
  $("#stage").style.setProperty(
    "--entry-opacity",
    String(ease((t - 21.9) / 0.13)),
  );
  $(".callout-rule").style.transform = `scaleX(${ease((t - 22.08) / 0.9)})`;
  const reveal = ease((t - 22) / 0.4),
    lift = ease((t - 26) / 1.8),
    zoom = 0.55 * ease((t - 27.3) / 1.65) + 0.45 * ease((t - 29.0) / 5.0);
  if (t >= 35) {
    setMode("detail");
    return undefined;
  }
  return { reveal, lift, zoom, time: t };
}

const inspectionOverlay = new InspectionOverlay();
const documentDecryption = new DocumentDecryption();
// A newly opened archive can introduce another font shard. Re-measure its
// redaction lines after font swap while retaining the current reveal progress.
document.fonts.addEventListener("loadingdone", () => documentDecryption.refresh());

let lastTime = 0,
  frameCount = 0,
  frameStart = performance.now(),
  fps = 0;
function frame(ms: number) {
  if (!wallpaperFrame(ms)) { requestAnimationFrame(frame); return; }
  if (document.hidden) { requestAnimationFrame(frame); return; }
  workbench?.tick();
  const realTime = ms / 1000;
  const time = animationTime(realTime);
  const theme = scene?.themeAmount ?? (prefs.colorTheme === "dark" ? 1 : 0);
  paintTheme(theme);
  playground?.tick(realTime);
  const cinema =
    mode === "boot" && ready
      ? bootFrame(openingTime(frozenTime ?? realTime - bootStart))
      : undefined;
  wallpaperEffects?.update(time, motionIsReduced(), motionActive("pointerParallax"));
  // The calibrated 2D opening fully covers the scene until array entry.
  if (!cinema || cinema.time >= 21.9) scene?.update(time, cinema);
  if (threeState === "closing" && scene?.presentationHidden) releaseThree();
  playground?.position();
  if (scene && mode === "detail") {
    documentDecryption.update(time, scene.decryptionFrame, !motionActive("documentReveal"));
    $("#detail-content").style.opacity = String(scene.detailVisibility);
    $("#detail-content").style.translate =
      `0 ${(1 - scene.detailVisibility) * 18}px`;
    $("#detail-content").inert = scene.detailVisibility < 0.1;
    if (pendingDetailFocus && scene.detailVisibility >= 0.1 && !modal) {
      $("#detail-content").focus({ preventScroll: true });
      pendingDetailFocus = false;
    }
  }
  $("#stage").style.setProperty("--detail-shade", String(mode === "boot" ? 0 : scene?.detailVisibility ?? 0));
  const currentScene = scene;
  if (currentScene) inspectionOverlay.render(currentScene.decryptionFrame,
    (x, y) => currentScene.projectCard(x, y), Boolean(cinema), motionActive("modelDecryption"));
  if (Math.floor(realTime) !== lastTime) {
    lastTime = Math.floor(realTime);
    updateFooterClock(new Date(), motionActive("rollingNumbers"));
  }
  frameCount++;
  if (ms - frameStart > 1000) {
    fps = (frameCount * 1000) / (ms - frameStart);
    frameStart = ms;
    frameCount = 0;
    $("#three-scene").dataset.fps = String(Math.round(fps));
    $("#three-scene").dataset.renderStats = JSON.stringify(scene?.getStats() ?? { loaded: false, drawCalls: 0, triangles: 0 });
  }
  requestAnimationFrame(frame);
}
function bindScene(scene: ArchiveScene, cell?: { lane: number; row: number }) {
    scene.select(selected, cell ? { cell } : undefined);
    scene.onSelect = (i, cell) => {
      if (mode !== "archive" || modal) return;
      select(i, cell ? { cell } : undefined);
    };
    scene.onNavigate = (axis, direction) => {
      if (mode !== "archive" || modal) return;
      if (axis === "lane") stepColumn(direction);
      else stepFile(direction);
    };
    scene.onHover = (i) => {
      const label = $("#hover-label");
      if (i === null) {
        label.hidden = true;
        hoverCode.finish();
        hoverTitle.finish();
        return;
      }
      const animated = motionActive("rollingText") && mode === "archive";
      const numbersAnimated = motionActive("rollingNumbers") && mode === "archive";
      hoverCode.update({
        value: Number(records[i].id.slice(2)),
        animated: !label.hidden && numbersAnimated,
      });
      hoverTitle.update({ text: records[i].title, animated: !label.hidden && animated });
      label.hidden = false;
      // Prepare the first visible value so the next hover can animate immediately.
      hoverCode.update({ animated: numbersAnimated });
      hoverTitle.update({ animated });
    };
}
function syncThreeButton() {
  $("#stage").dataset.threeState = threeState;
  syncWallpaperBackground();
  const button = document.querySelector<HTMLButtonElement>('[data-action="toggle-three"]');
  if (!button) return;
  button.textContent = threeState === "loading" ? "3D 载入中…" : threeState === "closing" ? "3D 关闭中…" : threeState === "off" ? "3D 关闭" : "3D 开启";
  button.disabled = threeState === "loading";
  button.setAttribute("aria-pressed", String(threeState === "on"));
  button.title = threeState === "off" ? "重新载入三维模型" : threeState === "closing" ? "取消关闭，恢复三维画面" : "卸载三维模型，保留 2D 界面";
}
function releaseThree() {
  if (!scene) return;
  resumeCell = { ...scene.getStats().selectedCell }; resumeSelection = selected;
  scene.dispose(); scene = undefined;
  if (mode === "detail") {
    $("#detail-content").style.opacity = "1";
    $("#detail-content").style.translate = "0 0";
    $("#detail-content").inert = false;
    documentDecryption.reset($("#detail-content"), true);
  }
  threeState = "off"; syncThreeButton();
  $("#hover-label").hidden = true;
  delete $("#three-scene").dataset.renderQuality;
  updateQualitySummary();
}
async function toggleThree() {
  if (!isWallpaper || !ready || threeState === "loading") return;
  if (threeState === "closing") {
    scene?.setPresentationVisible(true, !motionActive("surfaceTransitions"));
    threeState = "on"; syncThreeButton(); return;
  }
  if (scene) {
    playground?.stop();
    threeState = "closing"; syncThreeButton();
    scene.setPresentationVisible(false, !motionActive("surfaceTransitions"));
    if (!motionActive("surfaceTransitions")) releaseThree();
    return;
  }
  threeState = "loading"; syncThreeButton();
  let next: ArchiveScene | undefined;
  try {
    next = new ArchiveScene($("#three-scene"));
    next.renderer.domElement.style.opacity = "0";
    next.setPresentationVisible(false, true);
    await next.load();
    next.setMode(mode === "detail" ? "detail" : "archive");
    bindScene(next, resumeSelection === selected ? resumeCell : undefined);
    next.revealImmediately();
    scene = next;
    scene.setTheme(prefs.colorTheme === "dark", true);
    scene.setArchiveCoverage(wallpaperHost()?.properties.archivecoverage?.value === "extra");
    savePrefs();
    scene.setPresentationVisible(true, !motionActive("surfaceTransitions"));
    threeState = "on"; syncThreeButton();
  } catch (error) {
    next?.dispose(); scene = undefined;
    threeState = "off"; syncThreeButton();
    notify("三维模型载入失败，请点击 3D 关闭重试。");
    console.error(error);
  }
}

async function start() {
  try {
    if (isWallpaper) await window.rhineWallpaperPropertiesReady;
    if (!isWallpaper || wallpaperHost()?.properties.load3donstartup?.value !== false) {
      try {
        scene = new ArchiveScene($("#three-scene"));
        scene.setTheme(prefs.colorTheme === "dark", true);
        scene.setArchiveCoverage(wallpaperHost()?.properties.archivecoverage?.value === "extra");
      } catch { enableTwoDimensionalFallback(); }
    } else {
      threeState = "off";
      syncThreeButton();
    }
    await Promise.all([
      scene?.load().catch(() => { scene?.dispose(); scene = undefined; enableTwoDimensionalFallback(); }),
      loadBootWebfonts(),
      // With unicode-range faces, preload the opening's actual characters,
      // not every font shard. Other archive text loads on demand.
      document.fonts.load("300 20px MiSans", "欢迎使用 RInBot QQ机器人管理终端"),
      document.fonts.load("400 20px MiSans", "身份已确认请求已接收正在处理权限校验通过欢迎使用档案编号分组模块数据来源当前状态"),
      document.fonts.load("600 20px MiSans", "QQ 机器人框架本地运行与管理"),
      document.fonts.load("700 20px MiSans", "RInBot 系统档案管理终端"),
    ]);
    if (scene) bindScene(scene);
    savePrefs();
    ready = true;
    const initialRoute = currentManagementItem();
    const initialIndex = initialRoute ? records.findIndex((record) => record.title === initialRoute.title) : 0;
    select(initialIndex >= 0 ? initialIndex : 0);
    if (entry) entry.ready();
    else {
      if (isWallpaper) {
        // CEF allows automatic audio; never block the visual on audio policy or decoding.
        await Promise.race([audio.unlock(), new Promise(resolve => setTimeout(resolve, 3000))]);
      }
      completeStartup(false);
    }
  } catch (error) {
    console.error(error);
    $("#loading").innerHTML =
      '<div class="error-state"><strong>连接中断</strong><p>三维档案资源未能载入。请确认浏览器已启用硬件加速，然后重新连接。</p><button onclick="location.reload()">重新连接 →</button></div>';
  }
}
function enableTwoDimensionalFallback() {
  scene = undefined;
  threeState = "off";
  $("#stage").dataset.fallback = "2d";
  syncThreeButton();
}
function completeStartup(silent: boolean) {
  if (started || !ready) return;
  started = true;
  if (silent) {
    prefs.sound = false;
    prefs.music = false;
    saveAudioPrefs();
  }
  audio.releaseEntry();
  audio.restartBoot();
  const fade = motionActive("boot") ? openingDuration(600) : 0;
  loading.style.transitionDuration = `${fade}ms`;
  bootStart = performance.now() / 1000 - (reviewParams.has("time") ? Number(reviewParams.get("time")) : 1.76);
  if (!reviewParams.has("time")) bootStart += fade / 1000;
  setMode("boot");
  if (reviewParams.get("scene") === "archive" || (!motionActive("boot") && !reviewParams.has("time"))) setMode("archive");
  if (reviewParams.get("scene") === "detail") setMode("detail");
  const initialRoute = currentManagementItem();
  if (initialRoute) setMode("detail");
  if (isWallpaper && wallpaperHost()?.properties.boot?.value === false) setMode("archive");
  $("#stage").inert = false;
  $(".mobile-entry").inert = false;
  loading.classList.add("loaded");
  loading.inert = true;
  setTimeout(() => {
    const restoreFocus = loading.contains(document.activeElement) || document.activeElement === document.body;
    loading.remove();
    if (entry && restoreFocus) {
      const skip = $("#skip");
      const target = mode === "boot" ? skip.getClientRects().length ? skip : $(".mobile-entry") : $(".read-file");
      target.focus({ preventScroll: true });
    }
  }, fade);
  requestAnimationFrame(frame);
  // Do not compete with entry audio/font downloads. Full offline installation
  // begins after startup is complete and remains atomic.
  setTimeout(() => void initPwa(notify), 1500);
}
updateSelection();
const stopRuntime = startRuntime();
subscribeRuntime(() => {
  syncWorkspaceConnection();
  if (mode === "detail" && !modal) {
    const panel = $("#tab-panel");
    const active = document.activeElement;
    if (panel.contains(active) && (active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement || active instanceof HTMLSelectElement)) return;
    // A browser cannot restore a FileList after replacing its input.
    if (panel.dataset.structureDirty === "true" || panel.querySelector<HTMLInputElement>('input[type="file"]')?.files?.length) return;
    const state = capturePanelState(panel);
    panel.innerHTML = workspaceMarkup(resolveTab(records[selected].title, activeTab).view);
    restorePanelState(panel, state);
    syncWorkspaceConnection();
    documentDecryption.refresh();
  }
});
for (const eventName of ["input", "change"]) document.addEventListener(eventName, (event) => {
  const target = event.target;
  if ((target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement) && target.closest("#tab-panel")) target.dataset.dirty = "true";
});
document.addEventListener("rinbot-write-start", () => {
  document.querySelectorAll<HTMLInputElement>('#tab-panel input[type="password"]').forEach((input) => {
    if (/token$/.test(input.id)) { input.value = ""; delete input.dataset.dirty; }
  });
});
window.addEventListener("beforeunload", stopRuntime);
const customBackground = isWallpaper ? new WallpaperBackground($("#stage"), notify) : undefined;
function syncWallpaperBackground(retry = false) {
  customBackground?.update(wallpaperHost()?.properties ?? {}, mode !== "boot" && (threeState === "off" || threeState === "loading"), !motionActive("surfaceTransitions"), retry);
}
if (isWallpaper) {
  const apply = (properties: WallpaperProperties) => {
    const theme = properties.colortheme?.value;
    if (theme === "light" || theme === "dark") prefs.colorTheme = theme;
    scene?.setArchiveCoverage(properties.archivecoverage?.value === "extra" || wallpaperHost()?.properties.archivecoverage?.value === "extra");
    for (const key of ["sound", "music"] as const)
      if (typeof properties[key]?.value === "boolean") prefs[key] = properties[key].value as boolean;
    if (typeof properties.reduced?.value === "boolean") {
      prefs.motion = properties.reduced.value ? reducedMotion() : fullMotion();
      prefs.motionPreset = motionPresetFor(prefs.motion);
    }
    for (const key of ["soundVolume", "musicVolume"] as const) {
      const value = properties[key.toLowerCase()]?.value;
      if (typeof value === "number" && Number.isFinite(value)) prefs[key] = Math.max(0, Math.min(1, value / 100));
    }
    const qualityProperties = { ...wallpaperHost()?.properties, ...properties };
    if (Object.keys(properties).some(key => key === "renderquality" || key.startsWith("quality")))
      prefs.rendering = wallpaperQuality(qualityProperties, prefs.rendering);
    savePrefs();
    if (properties.customwallpaperfile || properties.customwallpaper?.value === true) syncWallpaperBackground(true);
    if (properties.boot?.value === false && started && mode === "boot") setMode("archive");
    // Keep an already-open settings surface in sync without replacing focused controls.
    document.querySelectorAll<HTMLInputElement>("[data-pref]").forEach(input => {
      const key = input.dataset.pref as "sound" | "music" | "quality" | "superPerformance";
      if (key in prefs) input.checked = prefs[key];
    });
    document.querySelectorAll<HTMLInputElement>("[data-motion]").forEach(input => {
      input.checked = prefs.motion[input.dataset.motion as MotionKey];
    });
    document.querySelectorAll<HTMLElement>('[data-action="motion-preset"]').forEach(button => {
      button.setAttribute("aria-pressed", String(button.dataset.preset === prefs.motionPreset));
    });
    const note = document.querySelector("#motion-preference-note");
    if (note) note.outerHTML = motionPreferenceNoteMarkup();
    for (const key of ["soundVolume", "musicVolume"] as const) {
      const input = document.querySelector<HTMLInputElement>(`[data-volume="${key}"]`);
      if (input) { input.value = String(Math.round(prefs[key] * 100)); input.closest("label")?.querySelector("output")?.replaceChildren(`${input.value}%`); }
    }
  };
  window.addEventListener("rhine-wallpaper-properties", event => apply((event as CustomEvent<WallpaperProperties>).detail));
  let pausedAt: number | undefined;
  const pause = () => {
    const paused = wallpaperHost()?.paused ?? false;
    if (paused && pausedAt === undefined) pausedAt = performance.now();
    if (!paused && pausedAt !== undefined) {
      if (started && mode === "boot") bootStart += (performance.now() - pausedAt) / 1000;
      pausedAt = undefined;
    }
    audio.setHostPaused(paused);
  };
  window.addEventListener("rhine-wallpaper-pause", pause);
  apply(wallpaperHost()?.properties ?? {});
  pause();
}
if (isWallpaper) {
  workbench = new Workbench($("#stage"), () => {
    if (ready && mode !== "boot") setMode("archive");
  }, lane => {
    if (ready && !modal) select(columnMemory[lane]);
  });
  workbench.setMotion(prefs.motion);
  playground = new ArchivePlayground($("#stage"), () => scene,
    () => ({ enabled: !!workbench?.enabled && mode === "archive" && ready, paused: Boolean(modal) || modalClosing || Boolean(wallpaperHost()?.paused) || document.hidden, reduced: motionIsReduced() }),
    value => { musicSuppressed = value; configureAudio(); }, () => audio.play("tick"));
  wallpaperEffects = new WallpaperEffects($("#stage"), () => scene);
  document.addEventListener("click", event => {
    const button = (event.target as Element).closest<HTMLElement>("[data-workbench-mode]");
    if (button) closeModal(() => { workbench!.setEnabled(button.dataset.workbenchMode === "workbench"); });
  });
}
void start();
// Deterministic review controls: the running application, never a video surrogate.
Object.assign(window, {
  rhine: {
    // The review button supplies a real user activation. Preferences stay local to this preview.
    playBootPreview: async (music = false) => {
      if (!ready || !navigator.userActivation.isActive) return false;
      const request = ++audioPreviewRequest;
      audioPreview = true;
      audio.configure({ ...prefs, sound: true, music });
      const unlocked = await audio.unlock();
      if (request !== audioPreviewRequest) return false;
      if (!unlocked) {
        audioPreview = false;
        configureAudio();
        return false;
      }
      replayBoot(true);
      return true;
    },
    seek: (t: number) => {
      setMode("boot");
      bootStart = performance.now() / 1000 - t;
      lastStep = "";
    },
    archive: () => setMode("archive"),
    detail: () => openFile(),
    select: (i: number) => select(i),
    stats: () => ({
      ...scene?.getStats(),
      threeState,
      fps: Math.round(fps),
      mode,
      ready,
      startup: started ? "started" : entry?.phase ?? "loading",
      motion: { reduced: motionIsReduced(), preset: prefs.motionPreset },
      bootTime: mode === "boot" ? started ? openingTime(frozenTime ?? performance.now() / 1000 - bootStart) + 5 : 6.76 : null,
      selected: records[selected].id,
      saved: [...saved],
      audio: audio.stats(),
      wallpaper: isWallpaper ? wallpaperHost() : null,
    }),
  },
});
if (import.meta.hot) import.meta.hot.dispose(() => audio.dispose());
