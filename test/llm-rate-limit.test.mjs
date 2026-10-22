import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-rate-limit-test-'));
process.env.QQ_AGENT_DATA_DIR = dir;
after(() => fs.rmSync(dir, { recursive: true, force: true }));
const { RequestScheduler, retryAfterMs, modelHttpError } = await import('../src/llm/request-scheduler.js');
const { DEFAULT_CONFIG, setRuntimeConfig } = await import('../src/core/config.js');
const { chatCompletion, chatCompletionWithRetry, isRetryableError } = await import('../src/llm/llm.js');
const { describeImages, visionModelOverride } = await import('../src/llm/vision-scan.js');
const api = { baseUrl: 'https://vision.test/v1', model: 'vision', apiKey: 'test' };
const deferred = () => {
  let resolve;
  const promise = new Promise((r) => { resolve = r; });
  return { promise, resolve };
};

test('reads provider cooldowns from seconds, HTTP dates and untruncated JSON', () => {
  const response = (value) => new Response('', { status: 429, headers: value ? { 'Retry-After': value } : {} });
  assert.equal(retryAfterMs(response('60'), '{}'), 60000);
  const now = Date.parse('2026-10-13T00:00:00Z');
  assert.equal(retryAfterMs(response('Tue, 13 Oct 2026 00:01:00 GMT'), '{}', now), 60000);
  const body = JSON.stringify({ error: { message: 'x'.repeat(900), retry_after: 90 } });
  const error = modelHttpError(response(), body);
  assert.equal(error.status, 429);
  assert.equal(error.retryAfterMs, 90000);
  assert.ok(error.message.length < 550);
  assert.equal(retryAfterMs(response('10'), '{"error":{"retry_after":20}}'), 20000);
  assert.equal(retryAfterMs(response('invalid'), '{"retry_after":-1}'), 60000);
});

test('vision requests are serial and enforce start spacing without serializing another model', async () => {
  let now = 0;
  const scheduler = new RequestScheduler({ now: () => now, sleep: async (ms) => { now += ms; } });
  const first = deferred();
  const starts = [];
  const options = { serialize: true, intervalMs: 2000 };
  const a = scheduler.run(api, options, async () => { starts.push(now); await first.promise; });
  await delay(0);
  const b = scheduler.run(api, options, async () => { starts.push(now); });
  await scheduler.run({ ...api, model: 'chat' }, options, async () => { assert.deepEqual(starts, [0]); });
  assert.deepEqual(starts, [0]);
  first.resolve();
  await Promise.all([a, b]);
  assert.deepEqual(starts, [0, 2000]);
});

test('queued callers share a 60-second cooldown instead of contacting the gateway', async () => {
  let now = 0;
  const waits = [];
  const scheduler = new RequestScheduler({ now: () => now, sleep: async (ms) => { waits.push(ms); now += ms; } });
  let calls = 0;
  const a = scheduler.run(api, { serialize: true }, async () => {
    calls++;
    throw Object.assign(new Error('模型 API HTTP 429'), { status: 429, retryAt: 60000 });
  });
  const b = scheduler.run(api, { serialize: true }, async () => { calls++; assert.equal(now, 60000); });
  const c = scheduler.run(api, { serialize: true }, async () => { calls++; assert.equal(now, 60000); });
  const results = await Promise.allSettled([a, b, c]);
  assert.deepEqual(results.map((r) => r.status), ['rejected', 'fulfilled', 'fulfilled']);
  assert.deepEqual(waits, [60000]);
  assert.equal(calls, 3);
});

test('cooldowns are scoped to provider, credential and model; long resets stay 429', async () => {
  const scheduler = new RequestScheduler({ now: () => 0, sleep: async () => { assert.fail('must not wait past budget'); } });
  await assert.rejects(scheduler.run(api, {}, async () => {
    throw Object.assign(new Error('模型 API HTTP 429'), { status: 429, retryAt: 3600000 });
  }), /429/);
  await assert.rejects(scheduler.run(api, { deadline: 90000 }, async () => assert.fail('still cooling down')),
    (error) => error.status === 429 && error.retryAfterMs === 3600000);
  for (const other of [{ ...api, model: 'chat' }, { ...api, apiKey: 'other' }, { ...api, baseUrl: 'https://other.test/v1' }]) {
    assert.equal(await scheduler.run(other, {}, async () => 'ok'), 'ok');
  }
});

test('a known cooldown beyond the queue budget stays 429 without waiting or sending', async () => {
  const scheduler = new RequestScheduler({ now: () => 0, sleep: async () => assert.fail('do not queue behind a known limit') });
  await assert.rejects(scheduler.run(api, {}, async () => {
    throw Object.assign(new Error('limited'), { status: 429, retryAt: 1000 });
  }));
  await assert.rejects(scheduler.run(api, { queueTimeoutMs: 10, deadline: 5000 }, async () => assert.fail('still cooling')),
    (error) => error.status === 429 && error.retryAt === 1000);
});

test('cancelling a queued request does not send it or allow another to bypass the active one', async () => {
  const scheduler = new RequestScheduler();
  const first = deferred();
  const starts = [];
  const a = scheduler.run(api, { serialize: true }, async () => { starts.push('first'); await first.promise; });
  await delay(0);
  const controller = new AbortController();
  const b = scheduler.run(api, { serialize: true, signal: controller.signal }, async () => assert.fail('cancelled'));
  const rejected = assert.rejects(b, /cancelled/);
  const c = scheduler.run(api, { serialize: true }, async () => starts.push('third'));
  controller.abort(new Error('cancelled'));
  await rejected;
  await delay(0);
  assert.deepEqual(starts, ['first']);
  first.resolve();
  await Promise.all([a, c]);
  assert.deepEqual(starts, ['first', 'third']);
});

test('cancelling during cooldown releases the queue without sending a request', async () => {
  const scheduler = new RequestScheduler();
  await assert.rejects(scheduler.run(api, {}, async () => {
    throw Object.assign(new Error('模型 API HTTP 429'), { status: 429, retryAt: Date.now() + 60000 });
  }), /429/);
  const controller = new AbortController();
  const request = scheduler.run(api, { serialize: true, signal: controller.signal }, async () => assert.fail('cooling down'));
  const rejected = assert.rejects(request, /abort|cancelled/i);
  await delay(0);
  controller.abort(new Error('cancelled'));
  await rejected;
});

let sequence = 0;
function configure() {
  const cfg = structuredClone(DEFAULT_CONFIG);
  cfg.api.baseUrl = `https://chat-${++sequence}.test/v1`;
  cfg.api.model = 'chat';
  cfg.api.thinking = 'off';
  cfg.api.visionModel = { enabled: true, provider: 'vision', model: 'vision', requestIntervalMs: 0, thinking: 'off' };
  cfg.providers = [{ id: 'vision', baseURL: `https://vision-${sequence}.test/v1` }];
  setRuntimeConfig(cfg);
  return cfg;
}
const ok = () => Response.json({ choices: [{ message: { content: 'ok' } }] });
function mockFetch(t, fn) {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  globalThis.fetch = fn;
}

function configureFallback() {
  const cfg = configure();
  cfg.api.timeoutMs = 5000;
  cfg.api.queueTimeoutMs = 10;
  cfg.api.fallback = { enabled: true, model: 'backup', timeoutMs: 5000 };
  setRuntimeConfig(cfg);
  return cfg;
}

function configureConservative() {
  const cfg = configureFallback();
  cfg.api.conservativeRequests = true;
  setRuntimeConfig(cfg);
  return cfg;
}

test('conservative requests do not retry or change models after rejections, limits or server failures', async (t) => {
  let status, calls;
  mockFetch(t, async () => {
    calls++;
    return new Response('thinking parameter rejected', { status });
  });
  for (status of [400, 401, 403, 429, 503]) {
    configureConservative();
    calls = 0;
    await assert.rejects(chatCompletionWithRetry({ messages: [] }), (error) => error.status === status);
    assert.equal(calls, 1, `one attempt for HTTP ${status}`);
    assert.equal(isRetryableError(new Error(`模型 API HTTP ${status}`)), false);
  }
});

test('conservative requests keep moderation refusals without stripping context and resubmitting', async (t) => {
  configureConservative();
  let calls = 0;
  mockFetch(t, async () => {
    calls++;
    return Response.json({ choices: [{ message: { content: 'This is a high risk request' } }] });
  });
  const result = await chatCompletionWithRetry({ messages: [{ role: 'user', content: 'test' }] });
  assert.match(result.message.content, /high risk/);
  assert.equal(calls, 1);
});

test('conservative chat keeps one timeout fallback', async (t) => {
  configureConservative();
  const models = [];
  mockFetch(t, async (_url, options) => {
    const model = JSON.parse(options.body).model;
    models.push(model);
    if (model === 'backup') return ok();
    return new Promise((resolve, reject) => {
      options.signal.addEventListener('abort', () => reject(options.signal.reason), { once: true });
    });
  });
  assert.equal((await chatCompletionWithRetry({ messages: [] })).message.content, 'ok');
  assert.deepEqual(models, ['chat', 'backup']);
});

test('conservative priority vision stops on limits and retains a single timeout fallback', async (t) => {
  let limited, models;
  mockFetch(t, async (_url, options) => {
    const model = JSON.parse(options.body).model;
    models.push(model);
    if (limited) return new Response('busy', { status: 429 });
    if (model === 'vision-backup') return ok();
    return new Promise((resolve, reject) => {
      options.signal.addEventListener('abort', () => reject(options.signal.reason), { once: true });
    });
  });
  for (limited of [true, false]) {
    const cfg = configureConservative();
    Object.assign(cfg.api.visionModel, { strategy: 'priority', models: ['vision-backup'],
      timeoutMs: 5000, fallbackTimeoutMs: 5000 });
    setRuntimeConfig(cfg);
    models = [];
    if (limited) {
      await assert.rejects(describeImages(['data:image/png;base64,test']), (error) => error.status === 429);
      await assert.rejects(describeImages(['data:image/png;base64,test']), (error) => error.status === 429);
      assert.deepEqual(models, ['vision']);
    } else {
      assert.equal(await describeImages(['data:image/png;base64,test']), 'ok');
      assert.deepEqual(models, ['vision', 'vision-backup']);
    }
  }
});

const restricted = () => Response.json({ error: {
  code: 'ACCOUNT_RESTRICTED', type: 'account_restricted', provider_code: 11140,
  retryable: false, message: 'Account restricted; manual recovery required'
} }, { status: 403 });

test('account restrictions retain provider metadata and stop same-credential fallback', async (t) => {
  configureFallback();
  const error = modelHttpError(restricted(), JSON.stringify({ error: {
    code: 'ACCOUNT_RESTRICTED', provider_code: 11140
  } }));
  assert.equal(error.code, 'MODEL_ACCOUNT_RESTRICTED');
  assert.equal(error.providerCode, 11140);
  assert.equal(error.retryable, false);
  const seen = [];
  mockFetch(t, async (_url, options) => { seen.push(JSON.parse(options.body).model); return restricted(); });
  await assert.rejects(chatCompletionWithRetry({ messages: [] }), (error) => error.code === 'MODEL_ACCOUNT_RESTRICTED');
  assert.deepEqual(seen, ['chat']);
});

test('an independently configured fallback credential remains eligible', async (t) => {
  const cfg = configureFallback();
  cfg.api.apiKey = 'primary-test-key';
  cfg.api.fallback.apiKey = 'independent-test-key';
  setRuntimeConfig(cfg);
  const seen = [];
  mockFetch(t, async (_url, options) => {
    const model = JSON.parse(options.body).model;
    seen.push(model);
    return model === 'chat' ? restricted() : ok();
  });
  assert.equal((await chatCompletionWithRetry({ messages: [] })).message.content, 'ok');
  assert.deepEqual(seen, ['chat', 'backup']);
});

test('ordinary request-level 403s retain their existing fallback policy', async (t) => {
  configureFallback();
  const seen = [];
  mockFetch(t, async (_url, options) => {
    const model = JSON.parse(options.body).model;
    seen.push(model);
    return model === 'chat' ? new Response('request content rejected', { status: 403 }) : ok();
  });
  await chatCompletionWithRetry({ messages: [] });
  assert.deepEqual(seen, ['chat', 'backup']);
});

test('vision does not try a second model on an account restriction', async (t) => {
  const cfg = configure();
  Object.assign(cfg.api.visionModel, { strategy: 'priority', models: ['vision-backup'] });
  setRuntimeConfig(cfg);
  let calls = 0;
  mockFetch(t, async () => { calls++; return restricted(); });
  await assert.rejects(describeImages(['data:image/png;base64,test']), (error) => error.code === 'MODEL_ACCOUNT_RESTRICTED');
  assert.equal(calls, 1);
});

test('both chat routes cooling down wait once and recover on the primary', async (t) => {
  configureFallback();
  const seen = [];
  let limitedAt = 0;
  mockFetch(t, async (_url, options) => {
    const model = JSON.parse(options.body).model;
    seen.push(model);
    if (seen.length <= 2) {
      limitedAt ||= Date.now();
      return new Response('busy', { status: 429, headers: { 'Retry-After': '0.08' } });
    }
    assert.ok(Date.now() - limitedAt >= 70, 'respect the shared provider cooldown');
    return ok();
  });
  const result = await chatCompletionWithRetry({ messages: [] });
  assert.equal(result.message.content, 'ok');
  assert.deepEqual(seen, ['chat', 'backup', 'chat']);
});

test('persistent rate limits get only one pool recovery round', async (t) => {
  configureFallback();
  const seen = [];
  mockFetch(t, async (_url, options) => {
    seen.push(JSON.parse(options.body).model);
    return new Response('busy', { status: 429, headers: { 'Retry-After': '0.08' } });
  });
  await assert.rejects(chatCompletionWithRetry({ messages: [] }), (error) => error.status === 429);
  assert.deepEqual(seen, ['chat', 'backup', 'chat', 'backup']);
});

test('pool recovery is cancellable and does not send after cancellation', async (t) => {
  configureFallback();
  const controller = new AbortController();
  let calls = 0;
  mockFetch(t, async () => {
    if (++calls === 2) setTimeout(() => controller.abort(new Error('cancelled')), 15);
    return new Response('busy', { status: 429, headers: { 'Retry-After': '0.08' } });
  });
  await assert.rejects(chatCompletionWithRetry({ messages: [], signal: controller.signal }), /cancelled|abort/i);
  assert.equal(calls, 2);
});

test('hour-long resets on both routes fail with 429 without a recovery round', async (t) => {
  configureFallback();
  let calls = 0;
  mockFetch(t, async () => {
    calls++;
    return new Response('busy', { status: 429, headers: { 'Retry-After': '3600' } });
  });
  await assert.rejects(chatCompletionWithRetry({ messages: [] }), (error) => error.status === 429);
  assert.equal(calls, 2);
});

test('the priority vision pool also recovers once after both routes cool down', async (t) => {
  const cfg = configure();
  Object.assign(cfg.api.visionModel, { models: ['vision-backup'], strategy: 'priority',
    queueTimeoutMs: 10, timeoutMs: 5000, fallbackTimeoutMs: 5000 });
  setRuntimeConfig(cfg);
  const seen = [];
  mockFetch(t, async (_url, options) => {
    seen.push(JSON.parse(options.body).model);
    return seen.length <= 2
      ? new Response('busy', { status: 429, headers: { 'Retry-After': '0.08' } })
      : ok();
  });
  assert.equal(await describeImages(['data:image/png;base64,test']), 'ok');
  assert.deepEqual(seen, ['vision', 'vision-backup', 'vision']);
});

test('a real 429 response delays retry and another vision request while main chat continues', async (t) => {
  configure();
  const first = deferred();
  const starts = [];
  let calls = 0;
  let limitedAt;
  mockFetch(t, async (_url, options) => {
    const body = JSON.parse(options.body);
    if (body.model === 'chat') return ok();
    starts.push(Date.now());
    if (++calls === 1) {
      first.resolve();
      await delay(5);
      limitedAt = Date.now();
      return Response.json({ error: { retry_after: 0.06 } }, { status: 429, headers: { 'Retry-After': '0.06' } });
    }
    assert.ok(Date.now() - limitedAt >= 55, 'no request during provider cooldown');
    return ok();
  });
  const a = describeImages(['data:image/png;base64,test']);
  await first.promise;
  const b = chatCompletion({ messages: [], overrides: visionModelOverride() });
  await chatCompletion({ messages: [] });
  assert.equal(calls, 1);
  await Promise.all([a, b]);
  assert.equal(calls, 3);
});

test('vision queue holds its turn until the response body is fully consumed', async (t) => {
  configure();
  const started = deferred();
  const finish = deferred();
  let calls = 0;
  mockFetch(t, async () => {
    calls++;
    if (calls > 1) return ok();
    started.resolve();
    return { ok: true, json: async () => { await finish.promise; return { choices: [{ message: { content: 'ok' } }] }; } };
  });
  const a = chatCompletion({ messages: [], overrides: visionModelOverride() });
  await started.promise;
  const b = chatCompletion({ messages: [], overrides: visionModelOverride() });
  await delay(0);
  assert.equal(calls, 1);
  finish.resolve();
  await Promise.all([a, b]);
  assert.equal(calls, 2);
});

test('an image scan with its default null signal can retry a transient network failure', async (t) => {
  configure();
  let calls = 0;
  mockFetch(t, async () => ++calls === 1 ? new Response('temporary', { status: 502 }) : ok());
  assert.equal(await describeImages(['data:image/png;base64,test']), 'ok');
  assert.equal(calls, 2);
});

test('a long provider quota reset is not retried within a shorter request budget', async (t) => {
  configure();
  let calls = 0;
  mockFetch(t, async () => { calls++; return new Response('quota reset later', { status: 429, headers: { 'Retry-After': '3600' } }); });
  await assert.rejects(chatCompletionWithRetry({ messages: [], overrides: visionModelOverride() }),
    (error) => error.status === 429 && error.retryAfterMs === 3600000);
  assert.equal(calls, 1);
});

test('automatic sticker collection does not restart model retries after a 429', async (t) => {
  const cfg = configure();
  cfg.sticker.autoCollect = true;
  cfg.sticker.enabled = true;
  cfg.security.allowPrivateImageHosts = true;
  setRuntimeConfig(cfg);
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAAAAAA6mKC9AAAAEElEQVR4nGNg+E8AMowoFQDA/X+BQnmBUwAAAABJRU5ErkJggg==', 'base64');
  let downloads = 0;
  const server = http.createServer((_req, res) => { downloads++; res.writeHead(200, { 'Content-Type': 'image/png' }); res.end(png); });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  let calls = 0;
  mockFetch(t, async () => { calls++; return new Response('limited', { status: 429, headers: { 'Retry-After': '3600' } }); });
  const { StickerManager } = await import('../src/onebot/sticker-manager.js');
  const manager = new StickerManager({ call: async () => [] });
  const result = await manager.autoCollect('group:1', {
    mid: 123, media: [{ kind: 'image', url: `http://127.0.0.1:${server.address().port}/test.png` }]
  });
  assert.equal(result, null);
  assert.equal(calls, 1);
  assert.equal(downloads, 1, 'outer loop must not redownload and restart the judge');
});
