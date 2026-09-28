// 本地回归：思考模式下的强制 tool_choice 降级。
//
// 背景（线上真实故障）：贴纸判断强制指定 tool_choice（要模型提交 submit_sticker_pick），
// 而 DeepSeek 系的思考模式不接受强制 tool_choice，整次请求直接 400
// "Thinking mode does not support this tool_choice"，重试 3 次后整张图被跳过。
// 现在 llm.js 在"思考开启 + 强制 tool_choice"时降级为 auto；
// 思考关闭时（如 purpose=chat）保持原样，因为那时强制值是合法的。
//
// 用法：QQ_AGENT_DATA_DIR=$(mktemp -d) node test/local/test-thinking-toolchoice.mjs
// 必须用临时数据目录，不能指向生产数据。
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

if (!process.env.QQ_AGENT_DATA_DIR || !fs.existsSync(process.env.QQ_AGENT_DATA_DIR)) {
  console.error('必须设置 QQ_AGENT_DATA_DIR 为已存在的临时目录（不要指向生产数据）');
  process.exit(2);
}

const dataDir = process.env.QQ_AGENT_DATA_DIR;
fs.writeFileSync(path.join(dataDir, 'config.json'), JSON.stringify({
  runtime: { mode: 'active', paused: false },
  allow: { private: ['100000001'] },
  allowAllWhenEmpty: true,
  api: {
    baseUrl: 'https://example.com/v1',
    apiKey: 'test-key',
    model: 'mock',
    // 聊天关思考、其余（判断类）保持开启 —— 与线上配置一致
    thinking: { chat: 'off', default: 'on' }
  }
}));

const { chatCompletion } = await import(new URL('../../src/llm/llm.js', import.meta.url).href);
const forced = { type: 'function', function: { name: 'submit_sticker_pick' } };
const tools = [{ type: 'function', function: { name: 'submit_sticker_pick', parameters: {} } }];

const originalFetch = globalThis.fetch;
const bodies = [];
globalThis.fetch = async (_url, request) => {
  bodies.push(JSON.parse(request.body));
  return Response.json({
    choices: [{ message: { content: '', tool_calls: [{ id: 't1', type: 'function', function: { name: 'submit_sticker_pick', arguments: '{}' } }] } }],
    usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 }
  });
};

const cases = [];
try {
  // 1) 判断类（不传 purpose → thinking on）：强制值必须降级为 auto
  bodies.length = 0;
  await chatCompletion({ messages: [{ role: 'user', content: '收还是不收？' }], tools, toolChoice: forced });
  cases.push(['思考开启 + 强制 tool_choice → 降级 auto', bodies[0]?.tool_choice === 'auto' && bodies[0]?.thinking === undefined]);

  // 2) 聊天类（purpose=chat → thinking off）：强制值保持原样，并带上关闭思考的字段
  bodies.length = 0;
  await chatCompletion({ messages: [{ role: 'user', content: '在吗' }], tools, toolChoice: forced, purpose: 'chat' });
  cases.push(['思考关闭 + 强制 tool_choice → 原样保留', bodies[0]?.tool_choice?.function?.name === 'submit_sticker_pick'
    && bodies[0]?.thinking?.type === 'disabled']);

  // 3) 默认的 auto 不受影响
  bodies.length = 0;
  await chatCompletion({ messages: [{ role: 'user', content: 'hi' }], tools });
  cases.push(['默认 auto 不受影响', bodies[0]?.tool_choice === 'auto']);
} finally {
  globalThis.fetch = originalFetch;
}

// ── 2026-09-28 审查跟进：400 摘除重试 / extraBody 优先级 / overrides 思考形状 ──
const { updateConfig } = await import(new URL('../../src/core/config.js', import.meta.url).href);
const ok200 = () => Response.json({
  choices: [{ message: { content: 'ok' }, finish_reason: 'stop' }],
  usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 }
});
const bad400 = (text) => new Response(
  JSON.stringify({ error: { message: text, type: 'invalid_request_error' } }),
  { status: 400 }
);
// responses 按次序消费，最后一个重复用（重试后成功的桩就是 [bad400, ok200]）
const makeFetch = (responses) => async (_url, request) => {
  bodies.push(JSON.parse(request.body));
  const make = responses.length > 1 ? responses.shift() : responses[0];
  return make();
};

try {
  // 4) 无关 400（错误体只有 invalid_request_error、不提 thinking/reasoning）→ 不摘参、不重试：
  //    旧判定里的 invalid/unknown/unexpected 泛词会误命中，白打一次注定失败的请求（审查 2026-09-28）
  updateConfig({ api: { baseUrl: 'https://api.deepseek.com/v1', thinking: 'low' } });
  bodies.length = 0;
  globalThis.fetch = makeFetch([() => bad400('This model maximum context length is 8192 tokens, however messages resulted in 9000 tokens')]);
  let err4 = '';
  try { await chatCompletion({ messages: [{ role: 'user', content: 'hi' }] }); } catch (e) { err4 = String(e?.message ?? e); }
  cases.push(['无关 400 不触发摘参重试（只发一次）', bodies.length === 1 && /模型 API HTTP 400/.test(err4)]);

  // 5) 思考参数真被拒（错误提到参数名）→ 摘掉思考参数重试一次，extraBody 原样保留
  updateConfig({ api: { baseUrl: 'https://api.deepseek.com/v1', thinking: 'low', extraBody: { top_p: 0.9 } } });
  bodies.length = 0;
  globalThis.fetch = makeFetch([() => bad400('Unknown parameter: reasoning_effort'), ok200]);
  const r5 = await chatCompletion({ messages: [{ role: 'user', content: 'hi' }] });
  cases.push(['思考参数被 400 拒绝 → 摘掉重试一次且保留 extraBody',
    bodies.length === 2
    && bodies[0]?.reasoning_effort === 'low' && bodies[0]?.top_p === 0.9
    && bodies[1]?.reasoning_effort === undefined && bodies[1]?.top_p === 0.9
    && r5.message?.content === 'ok']);

  // 6) 优先级链条：内置字段 < thinking.patch < extraBody。
  //    thinkingParams 的档位对象必须排在 max_tokens 之后（旧实现插在中间，两段语义不一致）
  updateConfig({
    api: {
      baseUrl: 'https://example.com/v1',
      thinking: 'low',
      thinkingParams: { low: { max_tokens: 123 } }
    }
  });
  bodies.length = 0;
  globalThis.fetch = makeFetch([ok200]);
  await chatCompletion({ messages: [{ role: 'user', content: 'hi' }], maxTokens: 500 });
  cases.push(['thinkingParams 档位排在内置字段之后（可覆盖 max_tokens）', bodies[0]?.max_tokens === 123]);

  // 7) overrides（记忆整理专用模型等）按 overrides 自己的地址取渠道形状：
  //    主渠道 Command Code（off=近似 reasoning_effort:low），专用 DeepSeek 应发 thinking.type=disabled
  updateConfig({ api: { baseUrl: 'https://api.commandcode.ai/provider/v1', thinking: 'off' } });
  bodies.length = 0;
  globalThis.fetch = makeFetch([ok200]);
  await chatCompletion({
    messages: [{ role: 'user', content: 'hi' }],
    purpose: 'judge',
    overrides: { baseUrl: 'https://api.deepseek.com/v1', apiKey: 'k2', model: 'm2', timeoutMs: 20000 }
  });
  cases.push(['overrides 专用模型按它自己的渠道取思考形状（不串主渠道）',
    bodies[0]?.thinking?.type === 'disabled' && bodies[0]?.reasoning_effort === undefined
    // extraBody 契约：overrides 未显式带 extraBody 时回落到配置里的 api.extraBody
    && bodies[0]?.top_p === 0.9]);
} finally {
  globalThis.fetch = originalFetch;
}

let ok = 0;
for (const [name, pass] of cases) {
  if (pass) ok += 1;
  console.log('%s %s', pass ? 'PASS' : 'FAIL', name);
}
console.log('结果: %d/%d', ok, cases.length);
process.exit(ok === cases.length ? 0 : 1);
