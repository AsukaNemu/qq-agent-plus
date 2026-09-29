// 群游戏管理器：生命周期、持久化（重启恢复）、消息拉取、效果派发与超时。
// 游戏规则在 ./games/<id>.js 插件里（只产出 effects：要发的话 / 要结束），发送与存盘都由这里统一做。
// 设计要点（docs/research/GAME_HOSTING_DESIGN.md）：私密信息（词/身份）只走私聊，公开摘要不含隐藏信息。
import fs from 'node:fs';
import path from 'node:path';
import { DATA_DIR, getConfig } from '../core/config.js';
import { sanitizeUserText, ZONE_OFFSET_MS } from '../core/util.js';
import * as numberBomb from './games/number-bomb.js';
import * as undercover from './games/undercover.js';

const FILE = path.join(DATA_DIR, 'games.json');
const PLUGINS = new Map([[numberBomb.meta.id, numberBomb], [undercover.meta.id, undercover]]);

const dayIndex = (ts) => Math.floor((Number(ts) + ZONE_OFFSET_MS) / 86400000);

export class GroupGameManager {
  constructor({ store, sender, log = console.log, now = () => Date.now(), rng = Math.random, wake = null } = {}) {
    this.store = store;
    this.sender = sender;
    this.log = log;
    this.now = now;
    this.rng = rng;
    this.wake = wake;
    this.games = new Map();     // chatKey -> { gameId, state, startedAt, lastSeenId, deadlineAt }
    this.daily = new Map();     // chatKey -> { dayKey, count }
    this.timer = null;
    this.#load();
  }

  #cfg() {
    const g = getConfig().groupGame || {};
    return {
      enabled: g.enabled === true,
      chats: Array.isArray(g.chats) ? g.chats.map((x) => String(x || '').trim()).filter(Boolean) : [],
      allowPrivateInvite: g.allowPrivateInvite === true,
      maxDurationMin: Math.min(180, Math.max(5, Number(g.maxDurationMin) || 60)),
      dailyLimitPerChat: Math.min(50, Math.max(1, Number(g.dailyLimitPerChat) || 6))
    };
  }

  #load() {
    try {
      const raw = JSON.parse(fs.readFileSync(FILE, 'utf8'));
      for (const [chatKey, item] of Object.entries(raw?.games || {})) {
        if (item && item.gameId && item.state) this.games.set(chatKey, item);
      }
      for (const [chatKey, d] of Object.entries(raw?.daily || {})) this.daily.set(chatKey, d);
    } catch { /* 首次运行没有文件 */ }
  }

  #save() {
    try {
      fs.mkdirSync(path.dirname(FILE), { recursive: true });
      fs.writeFileSync(FILE, JSON.stringify({
        games: Object.fromEntries(this.games),
        daily: Object.fromEntries(this.daily)
      }, null, 2));
      fs.chmodSync(FILE, 0o600);
    } catch (error) {
      this.log('[group-game] 存盘失败（不影响本局）:', error?.message ?? error);
    }
  }

  /** 启动推进循环（注意：与开局方法 start() 区分命名，JS 类里重名会互相覆盖）。 */
  startLoop() {
    if (this.timer) return;
    this.timer = setInterval(() => { this.tick().catch((e) => this.log('[group-game] tick 出错:', e?.message ?? e)); }, 20000);
    if (this.timer.unref) this.timer.unref();
    if (this.games.size) this.log(`[group-game] 已从磁盘恢复 ${this.games.size} 局进行中的游戏`);
  }

  stopLoop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  reconfigure() { /* 配置变更随时生效（每次读 getConfig），无需重启循环 */ }

  status(chatKey = '') {
    const list = [...this.games.entries()].map(([key, g]) => ({
      chatKey: key,
      game: g.gameId,
      name: PLUGINS.get(g.gameId)?.meta.name || g.gameId,
      startedAt: g.startedAt,
      summary: this.summaryFor(key, { includeBrief: false })
    }));
    return { enabled: this.#cfg().enabled, running: list.filter((x) => !chatKey || x.chatKey === chatKey) };
  }

  /** 注入提示词的公开摘要（不含任何隐藏信息）。 */
  summaryFor(chatKey, { includeBrief = true } = {}) {
    const cfg = this.#cfg();
    // 关掉开关或把群移出白名单后，不许再把"进行中的局"注入提示词（否则它会一直挂到重启）
    if (!cfg.enabled || !cfg.chats.includes(chatKey)) return '';
    const g = this.games.get(chatKey);
    if (!g) return '';
    const plugin = PLUGINS.get(g.gameId);
    if (!plugin) return '';
    const lines = [`【进行中的游戏】${plugin.meta.name}：`, plugin.summaryForModel(g.state)];
    if (includeBrief) lines.push(`主持要求：${plugin.hostBrief(g.state)}`);
    return lines.join('\n');
  }

  /** 开局。players 缺省用最近活跃成员；返回 { ok, text }（text 是可直接发给群里的说明）。 */
  async start({ chatKey, gameId, players = null }) {
    const cfg = this.#cfg();
    if (!cfg.enabled) return { ok: false, error: '群游戏未启用（控制台 → 实验性设置里打开）' };
    if (!cfg.chats.includes(chatKey)) return { ok: false, error: '这个群不在群游戏白名单里' };
    const plugin = PLUGINS.get(String(gameId || ''));
    if (!plugin) return { ok: false, error: `未知游戏：${gameId || '（没给）'}。可选：${[...PLUGINS.keys()].join(' / ')}` };
    if (this.games.has(chatKey)) return { ok: false, error: '这个群已经有一局在进行了（先 stop 或等它结束）' };
    if (plugin.meta.needsPrivate && !cfg.allowPrivateInvite) {
      return { ok: false, error: `${plugin.meta.name}需要私聊发词/身份：先到控制台打开「允许私聊发身份」` };
    }
    const now = this.now();
    const di = this.daily.get(chatKey);
    const today = dayIndex(now);
    const used = di && di.dayKey === today ? di.count : 0;
    if (used >= cfg.dailyLimitPerChat) return { ok: false, error: `今天这个群已经开过 ${used} 局了（上限 ${cfg.dailyLimitPerChat}），明天再来` };

    const active = this.store.activeMembers(chatKey, 50).map((m) => ({ userId: String(m.userId), name: m.name }));
    const activeIds = new Set(active.map((p) => p.userId));
    const idByName = new Map(active.map((p) => [String(p.name || '').trim(), p.userId]).filter(([k]) => k));
    const requested = Array.isArray(players) && players.length ? players : null;
    // 模型给的 players 必须先与本群已知成员求交集：只发给"报名者"这条不变量不能被
    // 一次提示注入绕过成"发给任意 QQ"（2026-09-28 审查 P3）。
    // 名单项支持 QQ 号或群名片——模型在消息里看得到名片、看不到号（2026-09-29 实测：
    // 用户说"就我们四个玩"时模型没法指定名单，默认会把最近发言的 9 人都卷进局）
    const resolvePlayer = (p) => {
      const uid = String(p?.userId ?? '').trim();
      const name = String(p?.name ?? p?.userId ?? '').trim();
      if (/^\d+$/.test(uid) && activeIds.has(uid)) return { userId: uid, name: name || uid };
      const hit = idByName.get(name);
      return hit ? { userId: hit, name: name || hit } : null;
    };
    const roster = (requested
      ? requested.map(resolvePlayer).filter(Boolean)
      : active)
      .slice(0, plugin.meta.maxPlayers);
    if (roster.length < plugin.meta.minPlayers) {
      return { ok: false, error: `${plugin.meta.name}至少要 ${plugin.meta.minPlayers} 个最近发过言的群友（现在只有 ${roster.length} 个）` };
    }
    const state = plugin.create({ players: roster, rng: this.rng, now });
    // 不带 readOnly：触发开局的那些消息此刻还是 leased，取 readOnly 会拿到更小的 id，
    // 下一轮 tick 会把它们再喂一遍（2026-09-28 审查 P3）
    const last = this.store.recent(chatKey, { limit: 1, includeSelf: true });
    const item = {
      gameId: plugin.meta.id,
      state,
      startedAt: now,
      lastSeenId: last.length ? Number(last[0].id) || 0 : 0,
      // 时长上限 = min(配置, 游戏自身上限)：之前直接用插件值，config.maxDurationMin 形同虚设
      deadlineAt: now + Math.min(Number(cfg.maxDurationMin) || 60, plugin.meta.maxDurationMin || 60) * 60000
    };
    this.games.set(chatKey, item);
    this.daily.set(chatKey, { dayKey: today, count: used + 1 });
    this.#save();

    const applied = await this.#applyEffects(chatKey, [
      ...(plugin.meta.id === 'undercover'
        ? [{ type: 'public', text: `🕵️ 谁是卧底开局（第 ${used + 1} 局 / 今日）：${roster.length} 人 —— `
            + roster.map((p, i) => `${i + 1}=${sanitizeUserText(String(p.name || p.userId))}`).join('、')
            + '。词已私聊给大家（没收到就悄悄说一声），别直接说出来；投票发「投 3」或「投 @他」。' }]
        : [{ type: 'public', text: `💣 数字炸弹开局：1~100 里藏了一个数，谁踩中谁输。直接发你猜的数字就行。` }]),
      ...(state.roles
        ? state.roles.map((r) => ({ type: 'private', userId: r.userId, text: `【谁是卧底】你的词是「${r.word}」。用一句话描述它（别说得太直白），轮到你时发出来。` }))
        : [])
    ]);
    // 成功/失败分开报给模型：之前只有失败计数，模型会把"部分成功"说成"全都没收到"
    // （2026-09-29 实测：管理员明明收到了词，模型却说"你们应该一条都没收到"）
    const privateOk = plugin.meta.needsPrivate ? roster.length - (applied?.privateFailed || 0) : 0;
    const text = `已开局：${plugin.meta.name}（${roster.length} 人）`
      + (plugin.meta.needsPrivate ? `，词已私聊发给 ${privateOk} 人` : '')
      + (applied?.privateFailed ? `；有 ${applied.privateFailed} 人没发出去（私聊被拒/不在私聊白名单），别再整体重发，谁说没收到就单独补发给谁` : '');
    return { ok: true, text };
  }

  async stop(chatKey, reason = '人工中止') {
    const g = this.games.get(chatKey);
    if (!g) return { ok: false, error: '这个群没有进行中的游戏' };
    const name = PLUGINS.get(g.gameId)?.meta.name || g.gameId;
    this.games.delete(chatKey);
    this.#save();
    await this.#applyEffects(chatKey, [{ type: 'public', text: `${name}到此为止（${reason}）。` }]);
    return { ok: true };
  }

  /** 处理一批新消息（由 tick 从 store 拉取，不侵入实时链路）。 */
  async handleNewMessages(chatKey) {
    const g = this.games.get(chatKey);
    if (!g) return;
    const plugin = PLUGINS.get(g.gameId);
    if (!plugin) { this.games.delete(chatKey); this.#save(); return; }
    // 按 lastSeenId 增量取（上限 400）：固定"最新 40 条"的窗口在积压超过 40 条时
    // 会静默丢掉最老的票/猜测，而 lastSeenId 又被推到最新，下一轮不再补
    // （2026-09-29 审查 P2；重启补课 + 实时消息叠加时最容易触发）
    const rows = this.store.recent(chatKey, { limit: 400, includeSelf: true, readOnly: true, afterId: Number(g.lastSeenId || 0) });
    if (!rows.length) return;
    let state = g.state;
    const effects = [];
    for (const m of rows) {
      g.lastSeenId = Math.max(Number(g.lastSeenId || 0), Number(m.id));
      if (m.self) continue;
      const out = plugin.onMessage(state, {
        userId: String(m.senderId || ''),
        name: sanitizeUserText(String(m.senderName || '')),
        text: String(m.text || ''),
        ts: Number(m.ts) || this.now()
      }, { now: this.now(), deadline: g.deadlineAt });
      state = out.state;
      effects.push(...(out.effects || []));
      if (state.phase === 'ended') break;
    }
    g.state = state;
    this.#save();
    if (effects.length) await this.#applyEffects(chatKey, effects);
    if (state.phase === 'ended') {
      this.games.delete(chatKey);
      this.#save();
    }
  }

  async tick() {
    const cfg = this.#cfg();
    if (!cfg.enabled) {
      if (this.games.size) {
        const dropped = this.games.size;
        this.games.clear();
        this.#save();
        this.log(`[group-game] 群游戏已关闭：清掉 ${dropped} 局进行中的游戏`);
      }
      return;
    }
    for (const [chatKey, g] of [...this.games.entries()]) {
      try {
        if (!cfg.chats.includes(chatKey)) {
          this.games.delete(chatKey);
          this.#save();
          this.log(`[group-game] ${chatKey} 已不在白名单，进行中的局就地结束`);
          continue;
        }
        if (this.now() >= g.deadlineAt) {
          await this.#applyEffects(chatKey, [{ type: 'end', result: '时间到了，这局就先到这里。' }]);
          this.games.delete(chatKey);
          this.#save();
          continue;
        }
        await this.handleNewMessages(chatKey);
        const cur = this.games.get(chatKey);
        if (!cur) continue;
        const plugin = PLUGINS.get(cur.gameId);
        const out = plugin.onTick(cur.state, { now: this.now(), deadline: cur.deadlineAt });
        cur.state = out.state;
        if (out.effects?.length) await this.#applyEffects(chatKey, out.effects);
        if (cur.state.phase === 'ended') {
          this.games.delete(chatKey);
          this.#save();
        }
      } catch (error) {
        this.log(`[group-game] ${chatKey} 推进出错（跳过这一轮）:`, error?.message ?? error);
      }
    }
  }

  async #applyEffects(chatKey, effects) {
    const cfg = this.#cfg();
    let privateFailed = 0;
    for (const effect of effects) {
      try {
        if (!effect) continue;
        if (effect.type === 'public' && effect.text) {
          await this.sender.sendTextBatch(chatKey, [String(effect.text).slice(0, 500)], {});
        } else if (effect.type === 'private' && effect.text) {
          if (!cfg.allowPrivateInvite) {
            privateFailed += 1;
            this.log('[group-game] 跳过私聊消息（未开启「允许私聊发身份」）');
            continue;
          }
          await this.sender.sendTextBatch(`private:${String(effect.userId)}`, [String(effect.text).slice(0, 500)], {});
        } else if (effect.type === 'wake' && this.wake) {
          this.wake(chatKey, Number(effect.delayMs) || 60000, String(effect.note || ''), { kind: 'game' });
        } else if (effect.type === 'end') {
          const g = this.games.get(chatKey);
          const name = PLUGINS.get(g?.gameId)?.meta.name || '游戏';
          await this.sender.sendTextBatch(chatKey, [`${name}结束：${String(effect.result || '')}`.slice(0, 500)], {});
        }
      } catch (error) {
        if (effect.type === 'private') privateFailed += 1;
        this.log('[group-game] 效果发送失败:', error?.message ?? error);
      }
    }
    return { privateFailed };
  }
}
