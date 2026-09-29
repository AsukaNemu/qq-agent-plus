// 群日报：每天固定时刻，把"昨天群里聊了啥"汇总成一条（模型生成、口语化）发到配置的群。
// 白名单制（groupDigest.chats 列表），默认关；发送走 sender 的正常通道（限频/留档与普通发言一致）。
import { getConfig } from '../core/config.js';
import { chatCompletion } from '../llm/llm.js';
import { nextAtFromHHMM } from '../core/reminders.js';
import { sanitizeUserText } from '../core/util.js';

const MIN_MESSAGES = 5;          // 少于这个数不生成（"昨天没人说话"没必要日报）
const WINDOW_MS = 24 * 60 * 60 * 1000;
const SAMPLE_CHARS = 2200;       // 喂给模型的聊天样本上限（省 token）

export class GroupDigestManager {
  constructor({ store, sender, log = console.log, now = () => Date.now() } = {}) {
    this.store = store;
    this.sender = sender;
    this.log = log;
    this.now = now;
    this.timer = null;
    this.running = false;
    this.lastRun = null;
  }

  #cfg() {
    const raw = getConfig().groupDigest || {};
    return {
      enabled: raw.enabled === true,
      time: String(raw.time || '09:30'),
      chats: Array.isArray(raw.chats) ? raw.chats.map((x) => String(x || '').trim()).filter(Boolean) : [],
      maxChars: Math.min(600, Math.max(80, Number(raw.maxChars) || 300))
    };
  }

  start() {
    this.reconfigure();
  }

  stop() {
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  reconfigure() {
    this.stop();
    const cfg = this.#cfg();
    if (!cfg.enabled) return;
    const at = nextAtFromHHMM(cfg.time, this.now());
    if (at === null) {
      this.log('[group-digest] 时间格式不合法（应为 HH:MM），已停用');
      return;
    }
    const wait = Math.max(1000, at - this.now());
    this.timer = setTimeout(() => {
      this.runOnce().catch((error) => this.log('[group-digest] 运行出错:', error?.message ?? error));
      this.reconfigure();   // 排下一天
    }, wait);
    if (this.timer.unref) this.timer.unref();
    this.log(`[group-digest] 已排程：${cfg.time}，群 ${cfg.chats.length} 个`);
  }

  status() {
    const cfg = this.#cfg();
    return {
      enabled: cfg.enabled,
      time: cfg.time,
      chats: cfg.chats,
      running: this.running,
      lastRun: this.lastRun
    };
  }

  /** 立即跑一轮（控制台手动触发/测试用）。返回每个群的执行结果。 */
  async runOnce() {
    if (this.running) return { skipped: 'already-running' };
    const cfg = this.#cfg();
    if (!cfg.chats.length) return { skipped: 'no-chats' };
    this.running = true;
    const results = [];
    try {
      for (const chatKey of cfg.chats) {
        try {
          const r = await this.#digestOne(chatKey, cfg);
          results.push({ chatKey, ...r });
        } catch (error) {
          const msg = String(error?.message ?? error).slice(0, 200);
          results.push({ chatKey, ok: false, error: msg });
          this.log(`[group-digest] ${chatKey} 失败：${msg}`);
        }
      }
      this.lastRun = { at: this.now(), results };
      return { ok: true, results };
    } finally {
      this.running = false;
    }
  }

  async #digestOne(chatKey, cfg) {
    if (!String(chatKey).startsWith('group:')) return { ok: false, error: '只支持群聊' };
    const since = this.now() - WINDOW_MS;
    const rows = this.store.recent(chatKey, { limit: 400, includeSelf: true, readOnly: true })
      .filter((m) => Number(m?.ts) >= since);
    const speakable = rows.filter((m) => String(m?.text || '').trim() !== '' && !/^\[/.test(String(m.text).trim()));
    if (rows.length < MIN_MESSAGES) return { ok: false, error: `消息太少（${rows.length} 条），跳过` };
    // 参与者统计（含机器人自己的发言）
    const byUser = new Map();
    for (const m of rows) {
      const uid = String(m?.senderId || '');
      if (!uid) continue;
      const cur = byUser.get(uid) || { name: sanitizeUserText(String(m?.senderName || uid)), count: 0 };
      cur.count += 1;
      byUser.set(uid, cur);
    }
    const top = [...byUser.values()].sort((a, b) => b.count - a.count).slice(0, 4);
    // 样本：从 tail 往前取，直到字符上限（越新越重要）
    const sample = [];
    let used = 0;
    for (const m of [...speakable].reverse()) {
      const line = `${m.self ? '我' : sanitizeUserText(String(m?.senderName || ''))}: ${String(m.text).replace(/\s+/g, ' ').slice(0, 120)}`;
      if (used + line.length > SAMPLE_CHARS) break;
      sample.push(line);
      used += line.length;
    }
    sample.reverse();
    const prompt = [
      '下面是一段群聊记录（最近的排在后）。请写一条"群日报"发给群里：用你的口吻，1~3 句短句（分条发就传数组的写法不需要，这里只要一条文本，句间用换行），',
      '概括这段时间群里聊了什么、有什么好玩的事；不列清单、不用"总结/日报如下"这类公文腔，像群友随口复盘。',
      `参与活跃：${top.map((t) => `${t.name}(${t.count}条)`).join('、')}；总消息 ${rows.length} 条。`,
      '',
      sample.join('\n')
    ].join('\n');
    const r = await chatCompletion({
      messages: [
        { role: 'system', content: '你是这个群里的普通群友，用口语聊天，不用 Markdown、不列条目、不写"大家"之类的客服腔。' },
        { role: 'user', content: prompt }
      ],
      temperature: 0.7,
      purpose: 'write'
      // 不设 maxTokens：思考模型（如 Command Code 上的 deepseek）的思考 token 也吃这个预算，
      // 压太小会出现"思考完预算没了 → content 为空"（2026-09-28 服务器实测）。输出长度用 maxChars 截。
    });
    const text = String(r.message?.content || '').trim().slice(0, cfg.maxChars);
    if (!text) return { ok: false, error: '模型没有产出内容' };
    await this.sender.sendTextBatch(chatKey, [text], {});
    return { ok: true, chars: text.length, messages: rows.length, text };
  }
}
