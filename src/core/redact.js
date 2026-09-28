// 日志/落盘文本的统一脱敏口径。
// 两个使用点：incident-pilot（异常面板入库前）、orchestrator（工具失败的 journal 行）——
// 后者会把工具错误原文写进 journald，而 OneBot 的 access_token 是挂在 URL 查询串上的
// （src/onebot/onebot.js 里拼接为 ?access_token=…），错误串里可能带上它，所以两边必须同一套规则。
export function redactText(value, max = 1000) {
  return String(value ?? '')
    .replace(/\bBearer\s+\S+/gi, 'Bearer [redacted]')
    // 查询串里的常见令牌参数名都要覆盖：只写 token/key 会漏掉本项目实际用的 access_token
    // 与常见的 api_key（`_token`/`_key` 这种带下划线前缀的形式，`[?&]token` 匹配不到）。
    .replace(/([?&](?:access_token|api_key|apikey|token|key|secret|password|authorization)=)[^&\s]+/gi, '$1[redacted]')
    .replace(/\0/g, '')
    .trim()
    .slice(0, max);
}
