// 渠道预设：模型连接的服务预设 + 各家「思考控制」参数的形状。
//
// 背景（2026-09-27 起因）：「关思考」的参数名各家不同，同一个模型经不同渠道
// 行为还会变（官方直连认 thinking，聚合网关可能吞掉、只认自己的 reasoning_effort）。
// 所以这里把「思考」拆成两层：
//   1) 语义层：用户/配置只表达 off / low / medium / high / max（provider-presets 负责归一化）；
//   2) 形状层：本表把语义翻译成具体请求参数，按「渠道」而不是「模型厂商」列。
//
// 硬规则：每条形状必须有出处（官方文档核过 / 本机实测，注明日期）；没核过的
// 宁可留空（uiLevels 不含该档、UI 置灰），也不猜——猜错就是 400 或静默失效。
// 用户遇到表外渠道时，走「测试思考能力」实测 +「额外请求参数」自定义，不依赖本表。

/** 语义档位的固定顺序（UI 排序、自定义映射的键都按它）。 */
export const LEVEL_ORDER = ['off', 'low', 'medium', 'high', 'max'];

/** 默认的「关思考」形状：历史行为（只发这一个字段的网关才认），保留为表外渠道的兜底。 */
export const DEFAULT_OFF_PATCH = { thinking: { type: 'disabled' } };

export const MODEL_SERVICES = [
  {
    id: 'deepseek',
    label: 'DeepSeek 官方',
    baseUrl: 'https://api.deepseek.com/v1',
    keyUrl: 'https://platform.deepseek.com/api_keys',
    hosts: ['api.deepseek.com'],
    source: '官方文档 2026-09-27：thinking 对象开关；reasoning_effort 支持 low / high / max',
    thinking: {
      canDisable: true,
      off: { thinking: { type: 'disabled' } },
      offIsApprox: false,
      efforts: {
        low: { reasoning_effort: 'low' },
        high: { reasoning_effort: 'high' },
        max: { reasoning_effort: 'max' }
      },
      uiLevels: ['off', 'low', 'high', 'max'],
      defaultNote: '默认开启思考、档位默认 high（官方文档）',
      note: '可关闭；档位 low / high / max。模型例：deepseek-flash（V4.1 Flash，支持图片）/ deepseek-v4-pro。'
    }
  },
  {
    id: 'zhipu',
    label: '智谱 GLM',
    baseUrl: 'https://open.bigmodel.cn/api/paas/v4',
    keyUrl: 'https://bigmodel.cn/usercenter/apikeys',
    hosts: ['open.bigmodel.cn', 'api.z.ai'],
    source: '官方 API 参考 2026-09-27：reasoning_effort 文本模型全集 max/xhigh/high/medium/low/minimal/none、默认 max；GLM-5.3/5.3-FLASH 仅 low/high/max；thinking.type 支持 enabled/disabled（GLM-4.5+，默认 enabled），GLM-5.3 系与 4.7/4.5V 强制思考',
    thinking: {
      canDisable: true,
      off: { thinking: { type: 'disabled' } },
      offIsApprox: false,
      // 官方枚举：GLM-5.3 系只认 low/high/max；GLM-5.2 及以上全集更大（下拉只暴露交集里最常用的三档）
      efforts: {
        low: { reasoning_effort: 'low' },
        high: { reasoning_effort: 'high' },
        max: { reasoning_effort: 'max' }
      },
      uiLevels: ['off', 'low', 'high', 'max'],
      defaultNote: '默认开启思考、档位默认 max（官方文档）',
      note: '档位 low/high/max（GLM-5.3 系官方枚举；5.2 及以上还有 medium/minimal/none 等，可用「额外请求参数」）。可关闭，但 GLM-5.3 系与 4.7/4.5V 强制思考、关不掉（选了会被安全兜底忽略）。模型例：glm-5.3-flash。'
    }
  },
  {
    id: 'qwen',
    label: '通义千问（百炼）',
    baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
    keyUrl: 'https://bailian.console.aliyun.com/',
    hosts: ['dashscope.aliyuncs.com'],
    source: '待核（enable_thinking 为社区通行写法，未逐字核官方文档）',
    thinking: {
      canDisable: null,
      off: { enable_thinking: false },
      offIsApprox: false,
      efforts: {
        low: { reasoning_effort: 'low' },
        medium: { reasoning_effort: 'medium' },
        max: { reasoning_effort: 'max' }
      },
      uiLevels: ['off', 'low', 'medium', 'max'],
      defaultNote: '混合思考模型默认值随模型；qwen3.8-omni-flash 默认开启、档位默认 xhigh',
      note: '官方文档：enable_thinking 可关（仅思考模式模型关不掉）；档位 reasoning_effort 仅 qwen3.8-omni-flash 支持（low/medium/xhigh 可直接选，我们的低/中/最高即 low/medium/max，官方把 high/max 映射到 xhigh）；其他模型的思考长度用 thinking_budget（token 预算，走「额外请求参数」）。模型例：qwen-max / qwen3-*。'
    }
  },
  {
    id: 'openai',
    label: 'OpenAI 官方',
    baseUrl: 'https://api.openai.com/v1',
    keyUrl: 'https://platform.openai.com/api-keys',
    hosts: ['api.openai.com'],
    source: '官方文档 2026-09-27：reasoning_effort 取值 none/minimal/low/medium/high/xhigh/max，随模型而定；none 可关闭推理，部分模型（如 GPT-6 Astra）传 none 会 400；默认值也随模型（gpt-5.5 默认 medium）',
    thinking: {
      canDisable: null,
      off: { reasoning_effort: 'none' },
      offIsApprox: false,
      efforts: {
        low: { reasoning_effort: 'low' },
        medium: { reasoning_effort: 'medium' },
        high: { reasoning_effort: 'high' },
        max: { reasoning_effort: 'max' }
      },
      uiLevels: ['off', 'low', 'medium', 'high', 'max'],
      defaultNote: '默认随模型（官方文档：如 gpt-5.5 默认 medium；部分模型不支持 none）',
      note: '档位与 none（关闭）随模型不同（官方文档）；不支持 none 的模型会 400，会被安全兜底自动去掉重试。模型例：gpt-5.x 系列。'
    }
  },
  {
    id: 'siliconflow',
    label: '硅基流动',
    baseUrl: 'https://api.siliconflow.cn/v1',
    keyUrl: 'https://cloud.siliconflow.cn/account/ak',
    hosts: ['api.siliconflow.cn'],
    source: '官方文档 2026-09-27：Qwen3 系支持 enable_thinking + thinking_budget（token 预算）',
    thinking: {
      canDisable: true,
      off: { enable_thinking: false },
      offIsApprox: false,
      // 官方文档：reasoning_effort 枚举仅 high | max（适用于 Pro/deepseek-ai/DeepSeek-V4、DeepSeek-V4-Flash、Pro/zai-org/GLM-5.2）
      efforts: {
        high: { reasoning_effort: 'high' },
        max: { reasoning_effort: 'max' }
      },
      uiLevels: ['off', 'high', 'max'],
      defaultNote: '推理模式下默认档位 high（官方文档；复杂 Agent 请求会自动用 max）',
      note: '官方文档：enable_thinking 可关、thinking_budget 限思维链长度（token 预算，走「额外请求参数」）；档位 reasoning_effort 仅 high/max（点名 DeepSeek-V4、V4-Flash、GLM-5.2；low/medium 会被官方映射到 high）。模型形如 deepseek-ai/DeepSeek-V3、Qwen/Qwen3-*。'
    }
  },
  {
    id: 'commandcode',
    label: 'Command Code（聚合网关 / Token Plan）',
    baseUrl: 'https://api.commandcode.ai/provider/v1',
    keyUrl: 'https://studio.commandcode.ai',
    hosts: ['api.commandcode.ai'],
    source: '本机实测 2026-09-27：吞 thinking 类参数；reasoning_effort 五档且无 off（none 被 400 拒绝）',
    thinking: {
      canDisable: false,
      // 关不掉——发最低档作近似，UI 会标注「近似关闭」
      off: { reasoning_effort: 'low' },
      offIsApprox: true,
      efforts: {
        low: { reasoning_effort: 'low' },
        medium: { reasoning_effort: 'medium' },
        high: { reasoning_effort: 'high' },
        max: { reasoning_effort: 'max' }
      },
      // 关不掉 → 界面不再提供「关闭」（用户要最低就选「低」，发的就是同一个值）；
      // off 的运行时映射保留：老配置里的 off 仍按最低档 low 发送，行为不变。
      uiLevels: ['low', 'medium', 'high', 'max'],
      defaultNote: '无法关闭；默认档位随模型（官方文档：不支持的档位会回落到模型默认）',
      note: '官方文档：档位 low/medium/high/xhigh/max（本机实测五个档位都被接受、无 off；官方写明不支持的档位会回落到模型默认，不会报错）——界面不提供「关闭」，要最低思考选「低」即可。'
    }
  },
  {
    id: 'opencode',
    label: 'OpenCode（Go 订阅）',
    baseUrl: 'https://opencode.ai/zen/go/v1',
    keyUrl: 'https://opencode.ai/',
    hosts: ['opencode.ai', 'api.opencode.ai'],
    source: '官方文档 2026-09-27：Go 订阅基址为 /zen/go/v1、模型前缀 opencode-go/、需 x-opencode-session 头（项目已自动附带）。思考行为未实测。',
    thinking: {
      canDisable: null,
      off: null,
      offIsApprox: false,
      efforts: {
        low: { reasoning_effort: 'low' },
        medium: { reasoning_effort: 'medium' },
        high: { reasoning_effort: 'high' },
        max: { reasoning_effort: 'max' }
      },
      uiLevels: ['low', 'medium', 'high', 'max'],
      defaultNote: '随模型（官方客户端文档：OpenAI 系约 none…xhigh；Anthropic 系 high 为默认）',
      note: '官方文档（客户端）：用 OpenAI 风格的 reasoningEffort（OpenAI 系约 none/minimal/low/medium/high/xhigh；Anthropic 系 thinking.budgetTokens）；但 Zen/Go 的 API 请求字段未文档化——先用「测试思考能力」实测；不接受会被安全兜底自动去掉。Go 订阅模型名形如 opencode-go/xxx；非 Go 订阅改地址为 https://opencode.ai/zen/v1。'
    }
  },
  {
    id: 'custom',
    label: '自定义 / 自建（OpenAI 兼容）',
    baseUrl: '',
    keyUrl: '',
    hosts: [],
    source: '-',
    thinking: {
      canDisable: null,
      off: null,
      offIsApprox: false,
      efforts: {},
      uiLevels: [],
      note: '表外渠道：思考参数不猜；用「额外请求参数」按你的服务商文档自定义。'
    }
  }
];

/** 归一化 baseUrl：小写、去首尾空白与结尾斜杠。 */
function normUrl(raw) {
  return String(raw || '').trim().toLowerCase().replace(/\/+$/, '');
}

/** 提取主机名（非法地址返回空串）。 */
export function hostOf(url) {
  try { return new URL(String(url || '').trim()).host.toLowerCase(); } catch { return ''; }
}

/** 按预设 id 取条目。 */
export function modelServiceById(id) {
  const key = String(id || '').trim().toLowerCase();
  if (!key) return null;
  return MODEL_SERVICES.find((s) => s.id === key) || null;
}

/** 按 baseUrl 的主机名匹配预设（表外返回 null）。 */
export function modelServiceOfBaseUrl(url) {
  const host = hostOf(url);
  if (!host) return null;
  return MODEL_SERVICES.find((s) => (s.hosts || []).includes(host)) || null;
}

/**
 * 把配置里的 thinking 值归一化成语义意图：
 * 'on' | 'off' | 'low' | 'medium' | 'high' | 'max'
 * 兼容历史形态：'on'/'off' 字符串、true/false、以及按用途对象 { chat: 'off', default: 'on' }。
 */
export function normalizeThinkingIntent(raw, purpose = '') {
  const pick = (v) => {
    if (v === 'off' || v === false) return 'off';
    if (v === 'low' || v === 'medium' || v === 'high' || v === 'max') return v;
    return 'on';
  };
  if (raw && typeof raw === 'object') {
    const v = purpose && raw[purpose] != null ? raw[purpose] : raw.default;
    return pick(v);
  }
  return pick(raw);
}

/**
 * 语义意图 → 请求参数补丁。
 * 返回 { patch, approx, suppressed } —— 或者 null（=不发任何思考参数）：
 *   - intent 'on'：null（保持服务商默认）；
 *   - intent 'off'：命中预设就用预设的 off 形状（Command Code 这类关不掉的会带 approx）；
 *     表外渠道沿用历史默认形状 { thinking: { type: 'disabled' } }；
 *     预设明确「不能关且没有近似」（如智谱）→ { patch:null, suppressed:true }，不发也不假装；
 *   - 档位（low/…）：预设里有该档才发；没有 → null（调用方会打一次性提示）。
 */
export function thinkingPatchFor(serviceId, intent) {
  const mode = normalizeThinkingIntent(intent);
  if (mode === 'on') return null;
  const service = modelServiceById(serviceId);
  if (mode === 'off') {
    const t = service?.thinking || {};
    // 有明确的关闭形状：照发（Command Code 这类会带 approx 标记）。
    if (t.off) return { patch: t.off, approx: t.offIsApprox === true, suppressed: false };
    // 官方明确不能关闭（如智谱）：不发、不假装。
    if (t.canDisable === false) return { patch: null, approx: false, suppressed: true };
    // 其余（表外渠道 / 未核过关闭参数的渠道）：沿用历史默认形状——
    // 升级前的行为就是「明确 off 才发 thinking:{type:'disabled'}」，这里不回归。
    return { patch: DEFAULT_OFF_PATCH, approx: false, suppressed: false };
  }
  const patch = service?.thinking?.efforts?.[mode] || null;
  if (!patch) return null;
  return { patch, approx: false, suppressed: false };
}

/**
 * 取某个地址对应的"思考设置原值"：优先按供应商（主机）存的独立设置，
 * 没有再退回全局 api.thinking（老配置照常工作）。
 * 每个供应商各自一条：换家不串味，也不会把别人家的选择重新解释。 */
export function effectiveThinkingRaw(apiCfg, host) {
  const key = String(host || '').toLowerCase();
  const map = apiCfg?.thinkingByService;
  if (key && map && typeof map === 'object' && map[key] != null) return map[key];
  return apiCfg?.thinking;
}

/**
 * 语义意图 → 请求参数补丁（含"自定义档位映射"）。
 * 表外/自定义渠道优先用用户自己的映射（api.thinkingParams，语义档位 → 请求字段）；
 * 其余情况沿用 thinkingPatchFor（内置表 → 历史默认）。
 */
export function resolveThinkingPatch(serviceId, intent, customParams) {
  const mode = normalizeThinkingIntent(intent);
  const service = modelServiceById(serviceId);
  const isCustom = !service || service.id === 'custom';
  if (isCustom && mode !== 'on' && customParams && typeof customParams === 'object') {
    const own = customParams[mode];
    if (own && typeof own === 'object' && !Array.isArray(own)) {
      return { patch: own, approx: false, suppressed: false };
    }
  }
  return thinkingPatchFor(serviceId, mode);
}

/** 某渠道在 UI 上应展示的档位：表内取预设；自定义/表外取用户映射的键（按固定顺序）。 */
export function thinkingLevelsFor(serviceId, customParams) {
  const service = modelServiceById(serviceId);
  const isCustom = !service || service.id === 'custom';
  if (isCustom && customParams && typeof customParams === 'object') {
    return LEVEL_ORDER.filter((lv) => customParams[lv] && typeof customParams[lv] === 'object');
  }
  return thinkingUiLevels(serviceId);
}

/** 某渠道在 UI 上应展示的档位（含 'off'；空数组 = 未验证，UI 只显示"默认"）。 */
export function thinkingUiLevels(serviceId) {
  const service = modelServiceById(serviceId);
  return service?.thinking?.uiLevels ? [...service.thinking.uiLevels] : [];
}
