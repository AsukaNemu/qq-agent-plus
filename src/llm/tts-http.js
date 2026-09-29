// TTS 各适配器共用的 HTTP 调用：统一超时、外部取消（signal）与"时间窗口"守卫。
// 单独一个文件是为了让 tts.js 与 tts-doubao.js 都能直接 import —— 之前把它当参数传进适配器，
// 会被 ops scan 当成"未定义调用点"误报（和插件里的 rng 同一类）。
import { watchTimeWindow } from '../core/time-gate.js';

export const DEFAULT_TIMEOUT_MS = 30000;

export async function callJson(fetchFn, url, { method = 'POST', headers = {}, body, timeoutMs, signal } = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(new Error('语音合成超时')), Math.max(5000, Number(timeoutMs) || DEFAULT_TIMEOUT_MS));
  const onAbort = () => controller.abort(signal?.reason ?? new Error('Run cancelled'));
  if (signal) {
    if (signal.aborted) controller.abort(signal.reason ?? new Error('aborted'));
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const releaseTimeGuard = watchTimeWindow((error) => controller.abort(error));
  try {
    const res = await fetchFn(url, {
      method,
      headers: { 'content-type': 'application/json', ...headers },
      body: JSON.stringify(body),
      signal: controller.signal
    });
    const text = await res.text();
    let parsed = null;
    try { parsed = JSON.parse(text); } catch { /* 下面按原文报错 */ }
    return { ok: res.ok, status: res.status, parsed, text };
  } finally {
    clearTimeout(timer);
    releaseTimeGuard();
    if (signal) signal.removeEventListener('abort', onAbort);
  }
}
