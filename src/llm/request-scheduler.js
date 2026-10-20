import { createHash } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';

export function isRateLimitError(error) {
  return error?.status === 429 || /HTTP\s*429\b/i.test(String(error?.message || ''));
}

// Keep the full response available for parsing even when the displayed message
// is truncated. Providers may put the delay in a header or in their JSON body.
export function retryAfterMs(response, text, now = Date.now()) {
  const header = response.headers?.get('retry-after');
  let headerMs = NaN;
  if (header !== null && header !== undefined && header.trim()) {
    const seconds = Number(header);
    headerMs = Number.isFinite(seconds)
      ? seconds * 1000 : Date.parse(header) - now;
  }
  let bodyMs = NaN;
  try {
    const body = JSON.parse(text);
    const value = body?.error?.retry_after ?? body?.retry_after;
    if (value !== null && value !== undefined && value !== '') bodyMs = Number(value) * 1000;
  } catch { /* A non-JSON error still has a usable Retry-After header. */ }
  const delays = [headerMs, bodyMs].filter((ms) => Number.isFinite(ms) && ms >= 0);
  return delays.length ? Math.max(1, ...delays) : 60000;
}

export function modelHttpError(response, text) {
  const error = new Error(`模型 API HTTP ${response.status}：${text.slice(0, 500)}`);
  error.status = response.status;
  if (response.status === 429) {
    error.retryAfterMs = retryAfterMs(response, text);
    error.retryAt = Date.now() + error.retryAfterMs;
  }
  return error;
}

function cooldownError(until, now) {
  const wait = Math.max(1, until - now);
  return Object.assign(new Error(`模型 API HTTP 429：模型冷却中，${Math.ceil(wait / 1000)} 秒后可重试`), {
    status: 429, retryAt: until, retryAfterMs: wait
  });
}

function waitForTurn(promise, signal) {
  signal?.throwIfAborted();
  if (!signal) return promise;
  return new Promise((resolve, reject) => {
    const abort = () => { cleanup(); reject(signal.reason); };
    const cleanup = () => signal.removeEventListener('abort', abort);
    signal.addEventListener('abort', abort, { once: true });
    promise.then(() => { cleanup(); resolve(); });
  });
}

export class RequestScheduler {
  constructor({ now = Date.now, sleep = (ms, signal) => delay(ms, undefined, { signal }) } = {}) {
    this.now = now;
    this.sleep = sleep;
    this.states = new Map();
  }

  stateKey(api) {
    const credential = createHash('sha256').update(String(api.apiKey || '')).digest('hex');
    return JSON.stringify([String(api.baseUrl || '').replace(/\/+$/, ''), credential, api.model]);
  }

  availability(api) {
    const state = this.states.get(this.stateKey(api));
    const now = this.now();
    return {
      pending: state?.pending || 0,
      cooldownMs: Math.max(0, (state?.cooldownUntil || 0) - now),
      waitMs: Math.max(0, (state?.cooldownUntil || 0) - now, (state?.nextStartAt || 0) - now)
    };
  }

  async run(api, { signal, serialize = false, intervalMs = 0, deadline = Infinity, queueTimeoutMs = 0 } = {}, task) {
    signal?.throwIfAborted();
    const key = this.stateKey(api);
    let state = this.states.get(key);
    if (!state) {
      state = { tail: Promise.resolve(), cooldownUntil: 0, nextStartAt: 0, pending: 0 };
      this.states.set(key, state);
    }
    state.pending++;
    let release = () => {};
    const previous = state.tail;
    if (serialize) {
      const turn = new Promise((resolve) => { release = resolve; });
      // Resolving a cancelled queued turn must not bypass its predecessor.
      state.tail = previous.then(() => turn);
    }
    const queueController = new AbortController();
    const queueSignal = signal ? AbortSignal.any([signal, queueController.signal]) : queueController.signal;
    const queueTimer = queueTimeoutMs > 0 ? setTimeout(() => {
      const now = this.now();
      const error = state.cooldownUntil > now ? cooldownError(state.cooldownUntil, now)
        : Object.assign(new Error(`模型排队超时（${queueTimeoutMs}ms）`), { code: 'MODEL_QUEUE_TIMEOUT' });
      queueController.abort(error);
    }, queueTimeoutMs) : null;
    try {
      if (serialize) await waitForTurn(previous, queueSignal);
      while (true) {
        queueSignal.throwIfAborted();
        const now = this.now();
        const remaining = Math.max(state.cooldownUntil, serialize ? state.nextStartAt : 0) - now;
        if (remaining <= 0) break;
        // A known limit beyond either budget stays 429, so routing can try
        // another model or wait for the pool without treating it as a timeout.
        if (state.cooldownUntil > now && (state.cooldownUntil >= deadline
          || (queueTimeoutMs > 0 && state.cooldownUntil - now >= queueTimeoutMs))) {
          throw cooldownError(state.cooldownUntil, now);
        }
        await this.sleep(Math.min(remaining, 2147483647), queueSignal);
      }
      clearTimeout(queueTimer);
      signal?.throwIfAborted();
      if (serialize) state.nextStartAt = this.now() + intervalMs;
      return await task();
    } catch (error) {
      if (queueController.signal.aborted && !signal?.aborted) error = queueController.signal.reason;
      if (isRateLimitError(error)) {
        state.cooldownUntil = Math.max(state.cooldownUntil,
          error.retryAt || this.now() + (error.retryAfterMs ?? 60000));
      }
      throw error;
    } finally {
      clearTimeout(queueTimer);
      state.pending--;
      release();
    }
  }
}

export const requestScheduler = new RequestScheduler();
