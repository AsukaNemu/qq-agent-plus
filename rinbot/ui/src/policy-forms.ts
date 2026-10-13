import { escapeHtml } from "./html.ts";
import type { BehaviorSettings, TimeControlSettings } from "./rinbot-runtime.ts";

export function parseList(value: string): string[] {
  return [...new Set(value.split(/[\s,，;；]+/).map((entry) => entry.trim()).filter(Boolean))];
}
export function behaviorFormMarkup(settings: BehaviorSettings): string {
  const lists = [["allow_groups", "允许回复的群号"], ["allow_private", "允许回复的私聊 QQ 号"], ["deny_groups", "禁止回复的群号"], ["deny_private", "禁止回复的私聊 QQ 号"]] as const;
  const counts = [["at_count", "被提及时上下文条数"], ["keyword_count", "关键词触发上下文条数"], ["random_count", "随机触发上下文条数"], ["all_count", "统一上下文条数"]] as const;
  return `<div class="runtime-inline"><div class="panel-label">回复范围与触发规则</div>
    ${lists.map(([key, label]) => `<label class="runtime-row"><span>${label}（空格或逗号分隔）</span><textarea id="behavior-${key}" rows="2">${escapeHtml(settings.access[key].join("\n"))}</textarea></label>`).join("")}
    <label class="runtime-row"><span>白名单为空时允许全部会话</span><input id="behavior-allow-all" type="checkbox" ${settings.access.allow_all_when_empty ? "checked" : ""} /></label>
    <label class="runtime-row"><span>普通消息随机回复概率（0–100%）</span><input id="behavior-random" type="number" min="0" max="100" value="${settings.triggers.random_percent}" /></label>
    <label class="runtime-row"><span>触发关键词（每行一个）</span><textarea id="behavior-keywords" rows="3">${escapeHtml(settings.triggers.keywords.join("\n"))}</textarea></label>
    <label class="runtime-row"><span>统一上下文条数</span><input id="behavior-unified" type="checkbox" ${settings.triggers.unified_tier ? "checked" : ""} /></label>
    ${counts.map(([key, label]) => `<label class="runtime-row"><span>${label}（1–500）</span><input id="behavior-${key}" type="number" min="1" max="500" value="${settings.triggers[key]}" /></label>`).join("")}
    <details><summary>群聊例外设置</summary>
      <label class="runtime-row"><span>群内屏蔽人物（群号 → QQ 号数组）</span><textarea id="behavior-blocked-users">${escapeHtml(JSON.stringify(settings.access.blocked_group_users, null, 2))}</textarea></label>
      <label class="runtime-row"><span>各群随机概率（群号 → 0–100）</span><textarea id="behavior-group-random">${escapeHtml(JSON.stringify(settings.triggers.group_random_percent, null, 2))}</textarea></label>
    </details><small>黑名单优先于白名单；这些规则只控制消息回复，不决定是否接受群邀请。</small>
    <label class="runtime-row"><span>管理员令牌</span><input id="behavior-admin-token" type="password" autocomplete="off" /></label>
    <button type="button" class="solid-button" data-action="save-behavior-settings">保存回复策略</button><small id="behavior-settings-status" role="status"></small></div>`;
}

export function readBehaviorForm(root: ParentNode, current: BehaviorSettings): BehaviorSettings {
  const value = (id: string) => root.querySelector<HTMLInputElement | HTMLTextAreaElement>(`#${id}`)?.value ?? "";
  const checked = (id: string) => root.querySelector<HTMLInputElement>(`#${id}`)?.checked ?? false;
  const result = structuredClone(current);
  for (const key of ["allow_groups", "allow_private", "deny_groups", "deny_private"] as const) {
    result.access[key] = parseList(value(`behavior-${key}`));
    if (result.access[key].some((id) => !/^\d{1,15}$/.test(id))) throw new Error("群号与 QQ 号只能填写数字");
  }
  result.access.allow_all_when_empty = checked("behavior-allow-all");
  result.access.blocked_group_users = JSON.parse(value("behavior-blocked-users"));
  result.triggers.group_random_percent = JSON.parse(value("behavior-group-random"));
  result.triggers.random_percent = Number(value("behavior-random"));
  if (!Number.isFinite(result.triggers.random_percent) || result.triggers.random_percent < 0 || result.triggers.random_percent > 100) throw new Error("随机回复概率须为 0–100%");
  result.triggers.keywords = value("behavior-keywords").split(/\r?\n/).map((item) => item.trim()).filter(Boolean);
  result.triggers.unified_tier = checked("behavior-unified");
  for (const key of ["at_count", "keyword_count", "random_count", "all_count"] as const) {
    result.triggers[key] = Number(value(`behavior-${key}`));
    if (!Number.isInteger(result.triggers[key]) || result.triggers[key] < 1 || result.triggers[key] > 500) throw new Error("上下文条数须为 1–500");
  }
  return result;
}

export function timeWindowMarkup(window = { days: [1, 2, 3, 4, 5, 6, 7], start: "00:00", end: "23:59" }) {
  return `<div class="incident-card time-window"><label class="runtime-row"><span>星期（1–7，逗号分隔）</span><input name="days" value="${escapeHtml(window.days.join(","))}" /></label><label class="runtime-row"><span>开始</span><input name="start" type="time" value="${escapeHtml(window.start)}" /></label><label class="runtime-row"><span>结束</span><input name="end" type="time" value="${escapeHtml(window.end)}" /></label><button type="button" data-action="remove-time-window">移除此时段</button></div>`;
}

export function timeFormMarkup(settings: TimeControlSettings): string {
  return `<div class="runtime-inline"><div class="panel-label">回复时间表 · 北京时间</div>
    <label class="runtime-row"><span>启用回复时间控制</span><input id="time-control-enabled" type="checkbox" ${settings.enabled ? "checked" : ""} /></label>
    <label class="runtime-row"><span>全局回复时段</span><select id="time-control-mode">${[["always", "全天"], ["deepseek-offpeak", "模型低峰时段"], ["custom", "自定义周计划"]].map(([value, label]) => `<option value="${value}" ${settings.schedule.mode === value ? "selected" : ""}>${label}</option>`).join("")}</select></label>
    <div id="time-windows">${settings.schedule.windows.map(timeWindowMarkup).join("")}</div><button type="button" data-action="add-time-window">添加自定义时段</button>
    <details><summary>会话单独设置</summary><label class="runtime-row"><span>会话覆盖（JSON）</span><textarea id="time-control-overrides">${escapeHtml(JSON.stringify(settings.overrides, null, 2))}</textarea></label><small>键使用 group:群号 或 private:QQ号；mode 为 inherit 表示沿用全局计划。</small></details>
    <label class="runtime-row"><span>管理员令牌</span><input id="time-control-admin-token" type="password" autocomplete="off" /></label>
    <button type="button" class="solid-button" data-action="save-time-control-settings">保存回复时间表</button><small id="time-control-settings-status" role="status"></small></div>`;
}

export function readTimeForm(root: ParentNode): TimeControlSettings {
  const mode = root.querySelector<HTMLSelectElement>("#time-control-mode")?.value ?? "always";
  const windows = [...root.querySelectorAll<HTMLElement>(".time-window")].map((row) => {
    const value = (name: string) => row.querySelector<HTMLInputElement>(`[name="${name}"]`)?.value ?? "";
    const days = parseList(value("days")).map(Number);
    const start = value("start"), end = value("end");
    if (!days.length || days.some((day) => !Number.isInteger(day) || day < 1 || day > 7) || !/^([01]\d|2[0-3]):[0-5]\d$/.test(start) || !/^([01]\d|2[0-3]):[0-5]\d$/.test(end)) throw new Error("请填写有效星期及开始、结束时间");
    return { days, start, end };
  });
  if (mode === "custom" && !windows.length) throw new Error("自定义周计划至少需要一个时段");
  return { enabled: root.querySelector<HTMLInputElement>("#time-control-enabled")?.checked ?? false, schedule: { mode, windows }, overrides: JSON.parse(root.querySelector<HTMLTextAreaElement>("#time-control-overrides")?.value ?? "{}") };
}
