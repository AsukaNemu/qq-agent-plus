// 思考控制（渠道预设 + 语义翻译）单测。
// 背景：各家「关思考/档位」参数名不同，同一模型经不同渠道行为还会变
// （官方直连认 thinking，聚合网关吞掉、只认自己的 reasoning_effort）。
// 这组测试钉住：语义归一化、按渠道翻译、表外渠道的历史默认行为不被破坏。
import assert from 'node:assert/strict';
import { test } from 'node:test';

const {
  MODEL_SERVICES, DEFAULT_OFF_PATCH,
  modelServiceById, modelServiceOfBaseUrl, normalizeThinkingIntent,
  thinkingPatchFor, thinkingUiLevels
} = await import('../src/core/provider-presets.js');

test('预设表完整性：每条都有出处，档位补丁都是普通对象', () => {
  const ids = new Set();
  for (const s of MODEL_SERVICES) {
    assert.ok(s.id && !ids.has(s.id), `id 缺失或重复: ${s.id}`);
    ids.add(s.id);
    assert.ok(s.label, `${s.id} 缺 label`);
    assert.ok(s.source, `${s.id} 缺 source（必须有出处：官方文档 / 实测）`);
    for (const [level, patch] of Object.entries(s.thinking?.efforts || {})) {
      assert.ok(['low', 'medium', 'high', 'max'].includes(level), `${s.id} 未知档位 ${level}`);
      assert.equal(typeof patch, 'object', `${s.id}.${level} 补丁应是对象`);
    }
    // 不能关闭的服务：要么不展示「关闭」，要么关闭是"近似"（如网关按最低档）
    if (s.thinking?.canDisable === false) {
      const hasOff = s.thinking.uiLevels.includes('off');
      assert.ok(!hasOff || s.thinking.offIsApprox === true,
        `${s.id}: 不能关闭却展示「关闭」且不是近似——不允许假装`);
    }
  }
  assert.ok(ids.has('commandcode') && ids.has('opencode'), '必须含 Command Code 与 OpenCode 预设');
});

test('按 baseUrl 主机名匹配渠道（含表外与非法地址）', () => {
  assert.equal(modelServiceOfBaseUrl('https://api.deepseek.com/v1')?.id, 'deepseek');
  assert.equal(modelServiceOfBaseUrl('https://api.deepseek.com')?.id, 'deepseek');
  assert.equal(modelServiceOfBaseUrl('https://open.bigmodel.cn/api/paas/v4')?.id, 'zhipu');
  assert.equal(modelServiceOfBaseUrl('https://api.z.ai/api/paas/v4')?.id, 'zhipu');
  assert.equal(modelServiceOfBaseUrl('https://api.commandcode.ai/provider/v1')?.id, 'commandcode');
  assert.equal(modelServiceOfBaseUrl('https://opencode.ai/zen/go/v1')?.id, 'opencode');
  assert.equal(modelServiceOfBaseUrl('https://dashscope.aliyuncs.com/compatible-mode/v1')?.id, 'qwen');
  assert.equal(modelServiceOfBaseUrl('https://my-own-gateway.example.com/v1'), null);
  assert.equal(modelServiceOfBaseUrl(''), null);
  assert.equal(modelServiceById('COMMANDCODE')?.id, 'commandcode');
  assert.equal(modelServiceById('nope'), null);
});

test('语义归一化：字符串/布尔/按用途对象，全部收敛到 on/off/档位', () => {
  assert.equal(normalizeThinkingIntent(undefined), 'on');
  assert.equal(normalizeThinkingIntent(null), 'on');
  assert.equal(normalizeThinkingIntent('on'), 'on');
  assert.equal(normalizeThinkingIntent(true), 'on');
  assert.equal(normalizeThinkingIntent('off'), 'off');
  assert.equal(normalizeThinkingIntent(false), 'off');
  for (const level of ['low', 'medium', 'high', 'max']) {
    assert.equal(normalizeThinkingIntent(level), level);
  }
  // 按用途对象：命中用途用用途值，其余落 default
  const obj = { chat: 'off', default: 'on' };
  assert.equal(normalizeThinkingIntent(obj, 'chat'), 'off');
  assert.equal(normalizeThinkingIntent(obj, 'judge'), 'on');
  assert.equal(normalizeThinkingIntent(obj, ''), 'on');
  const obj2 = { chat: 'low' };
  assert.equal(normalizeThinkingIntent(obj2, 'chat'), 'low');
  assert.equal(normalizeThinkingIntent(obj2, 'judge'), 'on');
});

test('按渠道翻译：能关的关、关不掉的近似、不能关的不假装、表外沿用历史默认', () => {
  // 表内可关（DeepSeek 官方）
  assert.deepEqual(thinkingPatchFor('deepseek', 'off'), { patch: { thinking: { type: 'disabled' } }, approx: false, suppressed: false });
  assert.deepEqual(thinkingPatchFor('deepseek', 'low'), { patch: { reasoning_effort: 'low' }, approx: false, suppressed: false });
  assert.deepEqual(thinkingPatchFor('deepseek', 'max'), { patch: { reasoning_effort: 'max' }, approx: false, suppressed: false });
  // 未核实的档位：不发（null），不能赌网关认不认
  assert.equal(thinkingPatchFor('deepseek', 'medium'), null);
  // 智谱：官方思考模式页写明 thinking.type 支持 disabled（GLM-5.3 系例外，靠安全兜底）
  assert.deepEqual(thinkingPatchFor('zhipu', 'off'), { patch: { thinking: { type: 'disabled' } }, approx: false, suppressed: false });
  assert.deepEqual(thinkingPatchFor('zhipu', 'max'), { patch: { reasoning_effort: 'max' }, approx: false, suppressed: false });
  // 官方枚举里 GLM-5.3 系支持的 low
  assert.deepEqual(thinkingPatchFor('zhipu', 'low'), { patch: { reasoning_effort: 'low' }, approx: false, suppressed: false });
  // 文档未列在暴露集里的档位不猜
  assert.equal(thinkingPatchFor('zhipu', 'medium'), null);
  // 聚合网关（实测关不掉）：按最低档近似，带 approx 标记
  assert.deepEqual(thinkingPatchFor('commandcode', 'off'), { patch: { reasoning_effort: 'low' }, approx: true, suppressed: false });
  assert.deepEqual(thinkingPatchFor('commandcode', 'high'), { patch: { reasoning_effort: 'high' }, approx: false, suppressed: false });
  // 未核实的渠道：关闭沿用历史默认（不回归），档位一律不发
  assert.deepEqual(thinkingPatchFor('opencode', 'off'), { patch: DEFAULT_OFF_PATCH, approx: false, suppressed: false });
  // OpenCode：官方客户端文档支持 reasoningEffort（OpenAI 风格）→ 档位照发；
  // Zen/Go 的 API 字段未文档化，靠「测试思考能力」实测 + 安全兜底。
  assert.deepEqual(thinkingPatchFor('opencode', 'low'), { patch: { reasoning_effort: 'low' }, approx: false, suppressed: false });
  assert.deepEqual(thinkingPatchFor('opencode', 'max'), { patch: { reasoning_effort: 'max' }, approx: false, suppressed: false });
  // 硅基流动：官方文档的 enable_thinking 开关
  assert.deepEqual(thinkingPatchFor('siliconflow', 'off'), { patch: { enable_thinking: false }, approx: false, suppressed: false });
  // OpenAI：none 可关闭（官方文档；不支持的模型靠安全兜底）
  assert.deepEqual(thinkingPatchFor('openai', 'off'), { patch: { reasoning_effort: 'none' }, approx: false, suppressed: false });
  assert.deepEqual(thinkingPatchFor('openai', 'max'), { patch: { reasoning_effort: 'max' }, approx: false, suppressed: false });
  // 通义：按待核的 enable_thinking:false 发
  assert.deepEqual(thinkingPatchFor('qwen', 'off'), { patch: { enable_thinking: false }, approx: false, suppressed: false });
  // 表外渠道 / 自定义：保留历史默认形状（只有 off 才发 thinking:disabled）
  assert.deepEqual(thinkingPatchFor('', 'off'), { patch: DEFAULT_OFF_PATCH, approx: false, suppressed: false });
  assert.equal(thinkingPatchFor('', 'low'), null);
  assert.deepEqual(thinkingPatchFor('custom', 'off'), { patch: DEFAULT_OFF_PATCH, approx: false, suppressed: false });
  // on 永远不发参数
  assert.equal(thinkingPatchFor('deepseek', 'on'), null);
  assert.equal(thinkingPatchFor('commandcode', 'on'), null);
});

test('UI 档位清单与翻译层一致：展示的档位都有补丁，没补丁的不展示', () => {
  for (const s of MODEL_SERVICES) {
    const levels = thinkingUiLevels(s.id);
    for (const level of levels) {
      if (level === 'off') continue; // off 的补丁可能来自 DEFAULT_OFF_PATCH（表外语义）
      const r = thinkingPatchFor(s.id, level);
      assert.ok(r && r.patch, `${s.id} 展示了档位 ${level} 却翻译不出补丁`);
    }
  }
  assert.deepEqual(thinkingUiLevels('deepseek'), ['off', 'low', 'high', 'max']);

  assert.deepEqual(thinkingUiLevels('commandcode'), ['low', 'medium', 'high', 'max']);  // 关不掉 → 不提供 Off
  assert.deepEqual(thinkingUiLevels('zhipu'), ['off', 'low', 'high', 'max']);
  assert.deepEqual(thinkingUiLevels(''), []);
});

test('每供应商独立设置：按主机取，未配置退回全局', async () => {
  const { effectiveThinkingRaw } = await import('../src/core/provider-presets.js');
  const apiCfg = {
    thinking: { chat: 'off', default: 'on' },
    thinkingByService: { 'api.commandcode.ai': 'low', 'open.bigmodel.cn': 'high' }
  };
  assert.equal(effectiveThinkingRaw(apiCfg, 'api.commandcode.ai'), 'low');
  assert.equal(effectiveThinkingRaw(apiCfg, 'API.CommandCode.AI'), 'low');  // 主机名大小写不敏感
  assert.equal(effectiveThinkingRaw(apiCfg, 'open.bigmodel.cn'), 'high');
  // 没有条目的供应商 → 全局兜底（老配置照常）
  assert.deepEqual(effectiveThinkingRaw(apiCfg, 'api.deepseek.com'), { chat: 'off', default: 'on' });
  assert.equal(effectiveThinkingRaw({ thinking: 'off' }, ''), 'off');
  assert.equal(effectiveThinkingRaw(undefined, 'x'), undefined);
});

test('自定义档位映射：表外渠道用用户映射，内置预设不受影响', async () => {
  const { resolveThinkingPatch, thinkingLevelsFor, LEVEL_ORDER } = await import('../src/core/provider-presets.js');
  const params = {
    low: { reasoning_effort: 'low' },
    high: { reasoning_effort: 'high', thinking: { type: 'enabled' } }
  };
  // 表外/自定义渠道：优先用户映射
  assert.deepEqual(resolveThinkingPatch('', 'low', params), { patch: { reasoning_effort: 'low' }, approx: false, suppressed: false });
  assert.deepEqual(resolveThinkingPatch('custom', 'high', params).patch, { reasoning_effort: 'high', thinking: { type: 'enabled' } });
  // 映射里没有的档位 → 退回表外默认（off 发历史形状；其他档位不发）
  assert.deepEqual(resolveThinkingPatch('', 'off', params).patch, { thinking: { type: 'disabled' } });
  assert.equal(resolveThinkingPatch('', 'medium', params), null);
  // 内置预设渠道：不受用户映射影响
  assert.deepEqual(resolveThinkingPatch('deepseek', 'low', params).patch, { reasoning_effort: 'low' });
  assert.deepEqual(resolveThinkingPatch('zhipu', 'low', params).patch, { reasoning_effort: 'low' });
  // 档位清单：自定义取映射键（固定顺序）；内置取预设
  assert.deepEqual(thinkingLevelsFor('', params), ['low', 'high']);
  assert.deepEqual(thinkingLevelsFor('', params), LEVEL_ORDER.filter((lv) => params[lv]).map((lv) => lv));
  assert.deepEqual(thinkingLevelsFor('siliconflow', params), ['off', 'high', 'max']);
  assert.deepEqual(thinkingLevelsFor('qwen', params), ['off', 'low', 'medium', 'max']);
});

test('按用途分设（{chat, default}）：逐用途解析到各自的档位', async () => {
  const { resolveThinkingPatch, normalizeThinkingIntent } = await import('../src/core/provider-presets.js');
  const perService = { chat: 'low', default: 'high' };
  assert.equal(normalizeThinkingIntent(perService, 'chat'), 'low');
  assert.equal(normalizeThinkingIntent(perService, 'judge'), 'high');
  // Command Code：low/high 都是已核实的档位
  assert.deepEqual(resolveThinkingPatch('commandcode', normalizeThinkingIntent(perService, 'chat')).patch, { reasoning_effort: 'low' });
  assert.deepEqual(resolveThinkingPatch('commandcode', normalizeThinkingIntent(perService, 'judge')).patch, { reasoning_effort: 'high' });
  // 'on' 表示该用途跟随服务商默认（不发参数）
  const mixed = { chat: 'on', default: 'off' };
  assert.equal(normalizeThinkingIntent(mixed, 'chat'), 'on');
  assert.equal(resolveThinkingPatch('commandcode', normalizeThinkingIntent(mixed, 'chat')), null);
  assert.deepEqual(resolveThinkingPatch('deepseek', normalizeThinkingIntent(mixed, 'judge')).patch, { thinking: { type: 'disabled' } });
});

test('任务用途（judge / write）逐用途解析，未配用途跟随 default', async () => {
  const { resolveThinkingPatch, normalizeThinkingIntent } = await import('../src/core/provider-presets.js');
  const per = { chat: 'low', judge: 'high', write: 'max', default: 'on' };
  assert.equal(normalizeThinkingIntent(per, 'judge'), 'high');
  assert.equal(normalizeThinkingIntent(per, 'write'), 'max');
  assert.equal(normalizeThinkingIntent(per, 'unknown-task'), 'on');   // 没配的用途 → default
  assert.deepEqual(resolveThinkingPatch('commandcode', normalizeThinkingIntent(per, 'judge')).patch, { reasoning_effort: 'high' });
  assert.deepEqual(resolveThinkingPatch('commandcode', normalizeThinkingIntent(per, 'write')).patch, { reasoning_effort: 'max' });
  assert.equal(resolveThinkingPatch('commandcode', normalizeThinkingIntent(per, 'unknown-task')), null);
});
