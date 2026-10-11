# 独立识图模型池

`api.visionModel.model` 保留原有单模型配置；`models` 可以加入同一提供商、
同一 API 地址和凭据下的额外识图模型。模型 ID 去空白、去重后，所有独立
识图请求按轮询顺序分配，包括聊天图片描述、表情收藏判断及其请求级重试。
状态查询和视觉能力检查不占用轮询次数，主聊天模型不参与这个池。

```json
{
  "api": {
    "visionModel": {
      "enabled": true,
      "provider": "your-provider-id",
      "model": "vision-model-a",
      "models": ["vision-model-a", "vision-model-b"],
      "balanceFrom": "2026-10-13T00:00:00+08:00",
      "thinking": "off",
      "timeoutMs": 90000
    }
  }
}
```

分配按请求次数，两个模型的调用数最多相差一次；token 数会随图片和返回
内容变化，不保证严格相等。轮询位置在进程内保存，重启后从首个模型开始，
不会为历史用量补偿分配。没有 `models` 或只有一个有效模型时保持单模型行为。

可选的 `balanceFrom` 是带时区的 ISO 日期：到该时间之前只使用 `model`，
到点后自动启用全部模型，无需定时重启。空值立即启用模型池；无效日期保守
保持单模型。`thinking: "off"` 单独关闭独立识图模型的思考，不受主聊天模型
或判断任务的通用思考档位影响；不填写时沿用已有的提供商思考设置。

`GET /api/vision/results` 的 `visionModel.models` 与 `distribution` 返回模型池
和分配方式，`activeModels` 返回当前实际启用的模型，`balanceFrom` 返回预约时间。
`data/usage-all.jsonl` 的 `requestedModel` 记录实际请求的模型 ID；
`model` 继续记录供应商返回的模型名，避免接入点被同一模型别名遮住。

恢复单模型：将 `api.visionModel.models` 设为 `[]`，保留原来的 `model`。
运行中的实例应通过配置 API 保存，或编辑配置后重启服务使其生效。
