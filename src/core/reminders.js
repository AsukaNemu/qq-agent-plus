// 定时提醒：群友让机器人"X 点提醒我 Y" —— 落盘持久化（重启不丢），到点走主动唤醒，
// 由模型用人设口吻把提醒说出来（而不是系统模板直发）。
// 与 schedule_wake 的区别：那个是"给自己排开口时机"、内存态、每会话只留一条；
// 这里是"给别人设的承诺"、持久化、可多条、可查询/取消。
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { DATA_DIR } from './config.js';
import { minuteOfDayInZone, sanitizeUserText } from './util.js';

const FILE = path.join(DATA_DIR, 'reminders.json');
export const MAX_REMINDER_TEXT = 200;
const MAX_PER_CHAT = 10;      // 单个会话待触发上限（防刷）
const MAX_PENDING = 50;       // 全局待触发上限
const DONE_KEEP = 5;          // 每个会话保留最近几条已完成记录（供 list 查看）
const MAX_DELAY_MS = 30 * 24 * 60 * 60 * 1000; // 最多 30 天后（再远没意义）
const EXPIRE_AFTER_MS = 12 * 60 * 60 * 1000;   // 离线导致迟到超过 12 小时 → 作废不补发

/** "HH:MM" → 下一次出现的绝对毫秒（按项目统一时区 UTC+8；已过则算明天）。非法返回 null。 */
export function nextAtFromHHMM(hhmm, now = Date.now()) {
  const m = /^(\d{1,2})[:：](\d{2})$/.exec(String(hhmm || '').trim());
  if (!m) return null;
  const hour = Number(m[1]);
  const minute = Number(m[2]);
  if (hour > 23 || minute > 59) return null;
  const nowMin = minuteOfDayInZone(now);
  const targetMin = hour * 60 + minute;
  let deltaMin = targetMin - nowMin;
  if (deltaMin <= 0) deltaMin += 24 * 60;   // 已过（或就是此刻）→ 明天同一时刻
  return now + deltaMin * 60 * 1000;
}

export class ReminderStore {
  constructor(file = FILE) {
    this.file = file;
    this.items = this.#load();
  }

  #load() {
    try {
      const raw = JSON.parse(fs.readFileSync(this.file, 'utf8'));
      return Array.isArray(raw?.items) ? raw.items.filter((x) => x && x.id && x.chatKey && x.at) : [];
    } catch {
      return [];
    }
  }

  #save() {
    try {
      fs.mkdirSync(path.dirname(this.file), { recursive: true });
      fs.writeFileSync(this.file, JSON.stringify({ items: this.items }, null, 2));
      fs.chmodSync(this.file, 0o600);
    } catch { /* 落盘失败不影响内存态 */ }
  }

  #prune() {
    // 每个会话只保留最近 DONE_KEEP 条已完成（fired/expired/canceled），pending 不剪
    const byChat = new Map();
    for (const it of this.items) {
      if (it.status === 'pending') continue;
      const list = byChat.get(it.chatKey) || [];
      list.push(it);
      byChat.set(it.chatKey, list);
    }
    const drop = new Set();
    for (const list of byChat.values()) {
      list.sort((a, b) => (b.finishedAt || 0) - (a.finishedAt || 0));
      for (const it of list.slice(DONE_KEEP)) drop.add(it.id);
    }
    if (drop.size) this.items = this.items.filter((it) => !drop.has(it.id));
  }

  /** 新增一条提醒。返回 { id, at }；超出限制/参数非法时抛带可读原因的错。 */
  add({ chatKey, at, text, createdBy = '' }) {
    const when = Number(at);
    // 提醒正文是"用户可诱导模型原样搬运"的文本，落盘前统一弱化段头（提示注入面，2026-09-28 审查 P2）
    const body = sanitizeUserText(String(text || '').trim()).slice(0, MAX_REMINDER_TEXT);
    if (!chatKey) throw new Error('缺少会话');
    if (!body) throw new Error('提醒内容不能为空');
    if (!Number.isFinite(when)) throw new Error('提醒时间不合法');
    const now = Date.now();
    if (when <= now + 5000) throw new Error('提醒时间必须晚于现在');
    if (when - now > MAX_DELAY_MS) throw new Error('最多只能设到 30 天后');
    const pending = this.items.filter((it) => it.status === 'pending');
    if (pending.length >= MAX_PENDING) throw new Error('待触发提醒太多（全局上限 50），先取消一些');
    if (pending.filter((it) => it.chatKey === chatKey).length >= MAX_PER_CHAT) {
      throw new Error('本会话待触发提醒已达上限（10 条），先取消一些');
    }
    const item = {
      id: crypto.randomBytes(4).toString('hex'),
      chatKey,
      at: when,
      text: body,
      createdBy: String(createdBy || '').slice(0, 40),
      status: 'pending',
      createdAt: now,
      firedAt: null,
      finishedAt: null
    };
    this.items.push(item);
    this.#save();
    return { id: item.id, at: item.at };
  }

  cancel({ id = '', chatKey = '' }) {
    const key = String(id || '').trim();
    // 不传 id 时：取消该会话**最近一条**待触发（"算了别提醒了"这种口语）。
    // 之前的写法 find(… && true) 恒命中数组里最早的一条，reverse 兜底是死代码
    // （2026-09-29 审查 P2：同一会话有两条 pending 时会取消错条）
    const hit = key
      ? this.items.find((it) => it.chatKey === chatKey && it.status === 'pending' && it.id === key)
      : [...this.items].reverse().find((it) => it.chatKey === chatKey && it.status === 'pending');
    if (!hit) return null;
    hit.status = 'canceled';
    hit.finishedAt = Date.now();
    this.#prune();
    this.#save();
    return hit;
  }

  list(chatKey, { includeDone = false } = {}) {
    return this.items
      .filter((it) => it.chatKey === chatKey && (includeDone || it.status === 'pending'))
      .sort((a, b) => a.at - b.at);
  }

  due(now = Date.now()) {
    return this.items.filter((it) => it.status === 'pending' && it.at <= now);
  }

  /** 迟到的提醒：超过 12 小时的直接作废（不补发一串"迟到的提醒"）。 */
  expired(now = Date.now()) {
    return this.items.filter((it) => it.status === 'pending' && it.at <= now - EXPIRE_AFTER_MS);
  }

  markFired(id, now = Date.now()) {
    const it = this.items.find((x) => x.id === id);
    if (!it) return null;
    it.status = 'fired';
    it.firedAt = now;
    it.finishedAt = now;
    this.#prune();
    this.#save();
    return it;
  }

  markExpired(id, now = Date.now()) {
    const it = this.items.find((x) => x.id === id);
    if (!it) return null;
    it.status = 'expired';
    it.finishedAt = now;
    this.#prune();
    this.#save();
    return it;
  }
}

export { EXPIRE_AFTER_MS };
