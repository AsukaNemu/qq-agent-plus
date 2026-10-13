import assert from 'node:assert/strict';
import { after, test } from 'node:test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'qq-vision-pool-test-'));
process.env.QQ_AGENT_DATA_DIR = dir;
after(() => fs.rmSync(dir, { recursive: true, force: true }));

const { DEFAULT_CONFIG, setRuntimeConfig } = await import('../src/core/config.js');
const { chatCompletion, chatCompletionWithRetry } = await import('../src/llm/llm.js');
const { describeImages, imageUnderstandingEnabled, visionModelOverride } = await import('../src/llm/vision-scan.js');
const { selectVisionModel } = await import('../src/llm/vision-model-pool.js');

let sequence = 0;
function configure(models = ['vision-a', 'vision-b']) {
  const cfg = structuredClone(DEFAULT_CONFIG);
  cfg.api.vision = true;
  cfg.api.model = 'text-main';
  cfg.api.thinking = 'off';
  cfg.api.visionModel = {
    enabled: true, provider: 'vision-provider', model: 'vision-a', models, requestIntervalMs: 0
  };
  cfg.providers = [{ id: 'vision-provider', baseURL: `https://vision-${++sequence}.example.test/v1` }];
  cfg.providerKeys = { 'vision-provider': 'test-key' };
  setRuntimeConfig(cfg);
  return cfg;
}

function mockRequests(t, respond = () => Response.json({
  model: 'shared-upstream-alias',
  choices: [{ message: { content: 'test description' } }],
  usage: { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 }
})) {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const models = [];
  globalThis.fetch = async (_url, options) => {
    const body = JSON.parse(options.body);
    models.push(body.model);
    assert.equal(body.visionModels, undefined);
    return respond(body, models.length);
  };
  return models;
}

test('legacy single-model configuration keeps its existing routing', async (t) => {
  configure([]);
  const seen = mockRequests(t);
  const override = visionModelOverride();
  assert.equal(override.visionModels, undefined);
  await chatCompletion({ messages: [], overrides: override });
  await chatCompletion({ messages: [], overrides: override });
  assert.deepEqual(seen, ['vision-a', 'vision-a']);
});

test('three idle vision routes are used in round-robin order', async (t) => {
  configure(['vision-a', 'vision-b', 'vision-c']);
  const seen = mockRequests(t);
  for (let i = 0; i < 6; i++) await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a', 'vision-b', 'vision-c', 'vision-a', 'vision-b', 'vision-c']);
});

test('a busy primary is skipped instead of making idle vision routes wait', async (t) => {
  configure(['vision-a', 'vision-b', 'vision-c']);
  let release;
  const blocked = new Promise((resolve) => { release = resolve; });
  t.after(() => release());
  let started;
  const firstStarted = new Promise((resolve) => { started = resolve; });
  const seen = mockRequests(t, async (body) => {
    if (body.model === 'vision-a') { started(); await blocked; }
    return Response.json({ choices: [{ message: { content: 'ok' } }] });
  });
  const first = chatCompletion({ messages: [], overrides: visionModelOverride() });
  await firstStarted;
  for (let i = 0; i < 3; i++) await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a', 'vision-b', 'vision-c', 'vision-b']);
  release();
  await first;
});

test('image descriptions can try all three routes and skip even hour-long cooldowns', async (t) => {
  configure(['vision-a', 'vision-b', 'vision-c']);
  const seen = mockRequests(t, (body) => body.model === 'vision-c'
    ? Response.json({ choices: [{ message: { content: 'recognized' } }] })
    : new Response('throttled', { status: 429, headers: { 'Retry-After': '3600' } }));
  assert.equal(await describeImages(['data:image/png;base64,test']), 'recognized');
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a', 'vision-b', 'vision-c', 'vision-c', 'vision-c']);
});

test('in-flight main chat using a pool model makes vision choose another model', async (t) => {
  const cfg = configure(['vision-a', 'vision-b', 'vision-c']);
  cfg.api.baseUrl = cfg.providers[0].baseURL;
  cfg.api.provider = 'vision-provider';
  cfg.api.model = 'vision-a';
  setRuntimeConfig(cfg);
  let release, started;
  const blocked = new Promise((resolve) => { release = resolve; });
  const firstStarted = new Promise((resolve) => { started = resolve; });
  t.after(() => release());
  const seen = mockRequests(t, async (body) => {
    if (body.model === 'vision-a') { started(); await blocked; }
    return Response.json({ choices: [{ message: { content: 'ok' } }] });
  });
  const chat = chatCompletion({ messages: [] });
  await firstStarted;
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a', 'vision-b']);
  release();
  await chat;
});

test('when every route is occupied, the shortest cooldown then shortest queue wins', () => {
  const api = { baseUrl: 'https://all-busy.test/v1', model: 'a', visionModels: ['a', 'b', 'c'] };
  const busy = { a: { pending: 0, waitMs: 60000 }, b: { pending: 2, waitMs: 0 }, c: { pending: 1, waitMs: 0 } };
  assert.equal(selectVisionModel(api, (candidate) => busy[candidate.model]).model, 'c');
  busy.b = { pending: 0, waitMs: 20000 };
  busy.c = { pending: 0, waitMs: 10000 };
  assert.equal(selectVisionModel(api, (candidate) => busy[candidate.model]).model, 'c');
});

test('a scheduled pool cannot bypass the schedule when its primary is throttled', async (t) => {
  const cfg = configure(['vision-a', 'vision-b', 'vision-c']);
  cfg.api.visionModel.balanceFrom = new Date(Date.now() + 86400000).toISOString();
  setRuntimeConfig(cfg);
  const seen = mockRequests(t, () => new Response('throttled', { status: 429, headers: { 'Retry-After': '3600' } }));
  await assert.rejects(chatCompletionWithRetry({ messages: [], overrides: visionModelOverride() }), /429/);
  assert.deepEqual(seen, ['vision-a']);
});

test('status checks do not allocate, while chat images and sticker judgments share allocation', async (t) => {
  const cfg = configure();
  const seen = mockRequests(t);
  for (let i = 0; i < 20; i++) {
    assert.equal(visionModelOverride().model, 'vision-a');
    assert.equal(imageUnderstandingEnabled(cfg), true);
  }
  for (let i = 0; i < 2; i++) {
    await describeImages(['data:image/png;base64,test']);
    await chatCompletionWithRetry({
      messages: [], purpose: 'judge', overrides: visionModelOverride()
    });
  }
  assert.deepEqual(seen, ['vision-a', 'vision-b', 'vision-a', 'vision-b']);
  assert.equal(cfg.api.model, 'text-main');
  assert.equal(cfg.api.visionModel.model, 'vision-a');
  const rows = fs.readFileSync(path.join(dir, 'usage-all.jsonl'), 'utf8')
    .trim().split('\n').map((line) => JSON.parse(line));
  assert.deepEqual(rows.slice(-4).map((row) => row.requestedModel), seen);
  assert.ok(rows.slice(-4).every((row) => row.model === 'shared-upstream-alias'));
});

test('concurrent requests are evenly distributed without mutating shared overrides', async (t) => {
  configure();
  const seen = mockRequests(t);
  const override = visionModelOverride();
  const responses = await Promise.all(Array.from({ length: 20 }, () =>
    chatCompletion({ messages: [], overrides: override })));
  assert.equal(seen.filter((model) => model === 'vision-a').length, 10);
  assert.equal(seen.filter((model) => model === 'vision-b').length, 10);
  assert.deepEqual(responses.map((response) => response.requestedModel), seen);
  assert.equal(override.model, 'vision-a');
});

test('request retries rotate to the next vision model', async (t) => {
  configure();
  const seen = mockRequests(t, (_body, count) => count === 1
    ? new Response('temporary unavailable', { status: 503 })
    : Response.json({ choices: [{ message: { content: 'ok' } }] }));
  const response = await chatCompletionWithRetry({
    messages: [], purpose: 'judge', overrides: visionModelOverride()
  }, 1);
  assert.deepEqual(seen, ['vision-a', 'vision-b']);
  assert.equal(response.requestedModel, 'vision-b');
});

test('empty, invalid and duplicate model IDs do not skew allocation', async (t) => {
  const cfg = configure(['', 'vision-a', ' vision-b ', 'vision-b', null, 12]);
  const seen = mockRequests(t);
  assert.deepEqual(visionModelOverride().visionModels, ['vision-a', 'vision-b']);
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a', 'vision-b']);
  assert.equal(visionModelOverride({ ...cfg,
    api: { ...cfg.api, visionModel: { ...cfg.api.visionModel, enabled: false } }
  }), null);
});

test('already cancelled requests do not consume an allocation', async (t) => {
  configure();
  const seen = mockRequests(t);
  await assert.rejects(chatCompletion({
    messages: [], overrides: visionModelOverride(),
    signal: AbortSignal.abort(new Error('test cancelled'))
  }), /test cancelled/);
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a']);
});

test('scheduled pool uses only the primary until the exact activation time', async (t) => {
  const cfg = configure();
  const startsAt = Date.now() + 86400000;
  cfg.api.visionModel.balanceFrom = new Date(startsAt).toISOString();
  setRuntimeConfig(cfg);
  const seen = mockRequests(t);
  const originalNow = Date.now;
  t.after(() => { Date.now = originalNow; });
  Date.now = () => startsAt - 1;
  const override = visionModelOverride();
  await chatCompletion({ messages: [], overrides: override });
  await chatCompletion({ messages: [], overrides: override });
  Date.now = () => startsAt;
  await chatCompletion({ messages: [], overrides: override });
  await chatCompletion({ messages: [], overrides: override });
  assert.deepEqual(seen, ['vision-a', 'vision-a', 'vision-a', 'vision-b']);
});

test('invalid activation dates keep additional endpoints disabled', async (t) => {
  const cfg = configure();
  cfg.api.visionModel.balanceFrom = 'invalid-date';
  setRuntimeConfig(cfg);
  const seen = mockRequests(t);
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  await chatCompletion({ messages: [], overrides: visionModelOverride() });
  assert.deepEqual(seen, ['vision-a', 'vision-a']);
});

test('vision thinking can be explicitly disabled while the main model still thinks', async (t) => {
  const cfg = configure([]);
  cfg.api.baseUrl = 'https://text.example.test/v1';
  cfg.api.thinking = 'on';
  cfg.api.thinkingByService = { [new URL(cfg.providers[0].baseURL).hostname]: 'on' };
  cfg.api.visionModel.thinking = 'off';
  setRuntimeConfig(cfg);
  const seen = mockRequests(t, (body) => {
    if (body.model === 'vision-a') {
      assert.deepEqual(body.thinking, { type: 'disabled' });
      assert.equal(body.messages[0].reasoning_content, undefined);
    } else {
      assert.equal(body.model, 'text-main');
      assert.equal(body.thinking, undefined);
      assert.equal(body.messages[0].reasoning_content, 'existing reasoning');
    }
    return Response.json({ choices: [{ message: { content: 'ok' } }] });
  });
  const messages = [{ role: 'assistant', content: 'previous', reasoning_content: 'existing reasoning' }];
  await chatCompletion({ messages, overrides: visionModelOverride() });
  await chatCompletion({ messages });
  assert.deepEqual(seen, ['vision-a', 'text-main']);
});
