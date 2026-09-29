import { getConfig } from './config.js';
import { isTimeActive } from './time-gate.js';

export function chatAllowed(chatKey, cfg = getConfig()) {
  const [kind, id] = String(chatKey).split(':');
  if (!['group', 'private'].includes(kind) || !/^\d+$/.test(id || '')) return false;
  const field = kind === 'group' ? 'groups' : 'private';
  if ((cfg.deny?.[field] || []).map(String).includes(id)) return false;
  const allow = (cfg.allow?.[field] || []).map(String);
  return allow.length ? allow.includes(id) : cfg.allowAllWhenEmpty === true;
}

export function canRun(chatKey) {
  const cfg = getConfig();
  return cfg.runtime?.mode === 'active' && !cfg.runtime?.paused
    && chatAllowed(chatKey, cfg) && isTimeActive(chatKey);
}

/**
 * 发送前置检查。
 *
 * `gameScoped`：**只**由群游戏管理器对"本局在册玩家"的私聊使用（报名 = 同意接收）——
 * 放宽的是 allow.private 这一条，**deny.private 仍然优先**（管理员屏蔽谁就发不进去），
 * 活跃时段/运行模式照旧。模型自己发消息（send_message / send_voice 工具）永远不带这个标记，
 * 所以"引擎文本只发给在册玩家"这条不变量不依赖模型自觉。
 */
export function assertCanSend(chatKey, signal, { gameScoped = false } = {}) {
  signal?.throwIfAborted();
  if (!isTimeActive(chatKey)) throw new Error('非活跃时间，禁止发送消息');
  const cfg = getConfig();
  const [kind, id] = String(chatKey).split(':');
  if (gameScoped && kind === 'private' && /^\d+$/.test(id || '')) {
    if (cfg.runtime?.mode !== 'active' || cfg.runtime?.paused) {
      throw new Error('Send blocked: observe/paused mode or chat not allowed');
    }
    if ((cfg.deny?.private || []).map(String).includes(id)) {
      throw new Error('Send blocked: 该用户被管理员屏蔽（deny 优先于游戏豁免）');
    }
    return;
  }
  if (!canRun(chatKey)) throw new Error('Send blocked: observe/paused mode or chat not allowed');
}
