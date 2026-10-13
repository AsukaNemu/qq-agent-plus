import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { archiveColumns, archiveSelection, columnFiles, managementNavigation, records, restoreSavedArchiveKeys } from "../src/data.ts";
import { adminPages, pageFor, resolveTab } from "../src/admin-pages.ts";
import { createMotionPreferences } from "../src/motion-preferences.ts";
import { ANIMATION_SPEED, NAVIGATION_SPEED, OPENING_SPEED, navigationDelta, navigationDuration, animationTime, openingTime } from "../src/animation-speed.ts";
import { DECRYPTION_END, DecryptionController } from "../src/decryption.ts";
import { damp } from "../src/motion.ts";

const recordKeys = [
  "abstract",
  "category",
  "dataSource",
  "en",
  "findings",
  "id",
  "key",
  "module",
  "status",
  "title",
].sort();

test("RInBot archive records contain only project-owned fields", () => {
  assert.equal(records.length, 22);
  for (const record of records) {
    assert.deepEqual(Object.keys(record).sort(), recordKeys);
    assert.match(record.id, /^X-\d{3}$/);
    assert.ok(archiveColumns.includes(record.category));
    assert.ok(record.findings.length > 0);
  }
});

test("archive labels and summaries do not regress to Rhine Lab sample content", () => {
  const displayedText = records
    .flatMap((record) => [
      record.title,
      record.en,
      record.category,
      record.module,
      record.dataSource,
      record.status,
      record.abstract,
      ...record.findings,
    ])
    .join("\n");

  for (const unrelatedLabel of ["莱茵生命", "Kristen Wright", "Saria", "科室", "相关人物"]) {
    assert.equal(displayedText.includes(unrelatedLabel), false, unrelatedLabel);
  }
});

test("archive identifiers and titles are unique", () => {
  assert.equal(new Set(records.map((record) => record.id)).size, records.length);
  assert.equal(new Set(records.map((record) => record.title)).size, records.length);
});

test("every visible archive owns operational tabs, not template documentation", () => {
  assert.deepEqual(new Set(records.map((record) => record.title)), new Set(Object.keys(adminPages)));
  for (const record of records) {
    const page = pageFor(record.title);
    assert.ok(page.tabs.length);
    assert.equal(new Set(page.tabs.map((tab) => tab.id)).size, page.tabs.length);
    assert.equal(resolveTab(record.title, "obsolete-tab").id, page.tabs[0].id);
    for (const tab of page.tabs) assert.equal(/实现说明|查看记录|研究记录|访问日志/.test(tab.label), false);
  }
});

test("archive display IDs are contiguous across ordered categories", () => {
  assert.deepEqual(records.map((record) => record.id), Array.from({ length: records.length }, (_, index) => `X-${String(index + 1).padStart(3, "0")}`));
  assert.equal(new Set(records.map((record) => record.key)).size, records.length);
  for (const category of archiveColumns) assert.ok(records.some((record) => record.category === category));
  let cursor = 0;
  for (let lane = 0; lane < archiveColumns.length; lane++) {
    const files = columnFiles(lane);
    assert.deepEqual(files, Array.from({ length: files.length }, (_, offset) => cursor + offset));
    for (const index of files) {
      const selection = archiveSelection(index);
      assert.equal(selection.total, 22);
      assert.equal(selection.number, Number(selection.record.id.slice(2)));
    }
    cursor += files.length;
  }
  for (const title of ["三维查看器", "字体与视觉资源", "图片降采样", "工作流", "消息去重"]) assert.equal(records.some((record) => record.title === title), false);
});

test("renumbering preserves old bookmarks without assigning them to another archive", () => {
  const keys = restoreSavedArchiveKeys(["X-043", "X-015", "X-002", null, "X-043"]);
  assert.deepEqual(records.filter((record) => keys.has(record.key)).map((record) => record.title).sort(), ["记忆整理", "贴纸管理"].sort());
  assert.equal(keys.size, 2);
  assert.equal(restoreSavedArchiveKeys({ malformed: true }).size, 0);
});

test("retired model viewer has no entry, runtime, CSS or motion settings", () => {
  const root = fileURLToPath(new URL("../src/", import.meta.url));
  for (const name of readdirSync(root).filter((name) => /\.(ts|css)$/.test(name))) {
    assert.doesNotMatch(readFileSync(`${root}/${name}`, "utf8"), /model-viewer|viewer-camera|viewerNavigation|viewerModelTransition|createAssemblyModel|createViewerPipeline|\.viewer-|360°/, name);
  }
  const motion = createMotionPreferences({ viewerNavigation: false, rollingNumbers: false }, false);
  assert.equal(Object.hasOwn(motion, "viewerNavigation"), false);
  assert.equal(motion.rollingNumbers, false);
});

test("six management groups expose working second-level destinations", () => {
  assert.deepEqual(managementNavigation.map((group) => group.label), [
    "总览", "会话", "能力", "自动化", "资源", "系统",
  ]);
  for (const group of managementNavigation) {
    assert.ok(group.items.length > 0, `${group.label} has destinations`);
    for (const item of group.items) {
      assert.ok(records.some((record) => record.title === item.title), `${group.label} / ${item.label}`);
    }
  }
});

test("page transitions stay at 3x while ordinary visual clocks return to 1x", () => {
  assert.equal(ANIMATION_SPEED, 1);
  assert.equal(NAVIGATION_SPEED, 3);
  assert.equal(animationTime(2), 2);
  assert.equal(navigationDuration(180), 60);
  assert.equal(navigationDelta(0.1, true), 0.1);
  const slow = { value: 0, velocity: 0 }, fast = { value: 0, velocity: 0 };
  damp(slow, 4.05, 4.2, 0.3);
  damp(fast, 4.05, 4.2, navigationDelta(0.1));
  assert.ok(Math.abs(slow.value - fast.value) < 1e-12);
});

test("opening runs independently at 1.5x without compounding UI speed", () => {
  assert.equal(OPENING_SPEED, 1.5);
  assert.equal(openingTime(2), 3);
  assert.equal(animationTime(2), 2);
  const main = readFileSync(new URL("../src/main.ts", import.meta.url), "utf8");
  assert.ok(main.includes("bootFrame(openingTime(frozenTime ?? realTime - bootStart))"));
  assert.ok(main.includes("bootStart += (performance.now() - pausedAt) / 1000"));
});

test("ordinary windows and number rolling keep original durations, without a global rate override", () => {
  const main = readFileSync(new URL("../src/main.ts", import.meta.url), "utf8");
  const clock = readFileSync(new URL("../src/rolling-clock.ts", import.meta.url), "utf8");
  const transitions = readFileSync(new URL("../src/ui-transitions.ts", import.meta.url), "utf8");
  assert.ok(main.includes("duration: 460"));
  assert.ok(clock.includes("duration: 460"));
  assert.ok(transitions.includes("private enterDuration = 300"));
  assert.ok(transitions.includes("private exitDuration = 200"));
  assert.equal(main.includes("installAnimationSpeed"), false);
  assert.ok(main.includes("new ContentTransition(navigationDuration(150))"));
});

test("decryption finishes in one third of the time without dropping reduced motion", () => {
  const elapsedUntilClear = (speed) => {
    const controller = new DecryptionController();
    controller.enter(); controller.update(0, true, false);
    let frames = 0;
    while (controller.frame.phase !== "clear" && frames < 1000) {
      controller.update(speed / 60, true, false); frames++;
    }
    assert.equal(controller.frame.phase, "clear");
    return frames / 60;
  };
  assert.ok(Math.abs(elapsedUntilClear(1) - 3 * elapsedUntilClear(NAVIGATION_SPEED)) < 0.05);
  const reduced = new DecryptionController(); reduced.enter(); reduced.update(0, false, true);
  assert.equal(reduced.frame.time, DECRYPTION_END);
  const main = readFileSync(new URL("../src/main.ts", import.meta.url), "utf8");
  assert.ok(main.includes("Math.floor(realTime)"));
  assert.ok(main.includes("updateFooterClock(new Date()"));
});
