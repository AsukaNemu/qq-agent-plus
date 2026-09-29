// OpenAI 兼容的语音合成（POST {base}/audio/speech），供「发语音」工具使用。
// 与 ASR 侧的多供应商结构对称：这里先只做兼容端点（硅基流动 / OpenAI / 自建网关），
// 原生厂商（火山/讯飞）等有需求再加适配器。
import { watchTimeWindow } from '../core/time-gate.js';

export function ttsConfigured(cfg) {
  const t = cfg?.tts || {};
  return t.enabled === true && String(t.baseUrl || '').trim() !== '';
}

/**
 * 合成语音。返回 { buffer, format }；失败抛带可读原因的错。
 * cfg: { baseUrl, apiKey, model, voice, format, timeoutMs }
 */
export async function synthesizeSpeech({
  cfg,
  text,
  signal = null,
  fetchFn = fetch
} = {}) {
  const base = String(cfg?.baseUrl || '').trim().replace(/\/+$/, '');
  const apiKey = String(cfg?.apiKey || '').trim();
  const model = String(cfg?.model || '').trim();
  const voice = String(cfg?.voice || '').trim() || 'alloy';
  const format = String(cfg?.format || 'mp3').trim() || 'mp3';
  const body = String(text || '').trim().slice(0, 300);
  // 语速/增益：硅基流动实测真实生效（speed 0.7→8.1s / 1.3→4.7s，同一句）；越界先夹紧再发
  const speed = Number(cfg?.speed);
  const gain = Number(cfg?.gain);
  const extras = {};
  if (Number.isFinite(speed) && speed !== 1) extras.speed = Math.min(4, Math.max(0.25, speed));
  if (Number.isFinite(gain) && gain !== 0) extras.gain = Math.min(10, Math.max(-10, gain));
  if (!base) throw new Error('未配置语音合成的 Base URL（设置 → 语音回复）');
  if (!model) throw new Error('未配置语音合成模型（例如 FunAudioLLM/CosyVoice2-0.5B）');
  if (!body) throw new Error('要合成的文本为空');

  const controller = new AbortController();
  const timeoutMs = Math.max(5000, Number(cfg?.timeoutMs) || 30000);
  const timer = setTimeout(() => controller.abort(new Error('语音合成超时')), timeoutMs);
  const onAbort = () => controller.abort(signal?.reason ?? new Error('Run cancelled'));
  if (signal) {
    if (signal.aborted) controller.abort(signal.reason ?? new Error('aborted'));
    else signal.addEventListener('abort', onAbort, { once: true });
  }
  const releaseTimeGuard = watchTimeWindow((error) => controller.abort(error));
  try {
    const res = await fetchFn(`${base}/audio/speech`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {})
      },
      body: JSON.stringify({ model, input: body, voice, response_format: format, ...extras }),
      signal: controller.signal
    });
    if (!res.ok) {
      let detail = '';
      try { detail = String(await res.text()).slice(0, 200); } catch { /* 忽略 */ }
      throw new Error(`语音合成失败 HTTP ${res.status}${detail ? `：${detail}` : ''}`);
    }
    const buffer = Buffer.from(await res.arrayBuffer());
    if (!buffer.length) throw new Error('语音合成返回了空音频');
    if (buffer.length > 4 * 1024 * 1024) throw new Error('语音合成结果过大（>4MB）');
    return { buffer, format };
  } finally {
    clearTimeout(timer);
    releaseTimeGuard();
    if (signal) signal.removeEventListener('abort', onAbort);
  }
}
