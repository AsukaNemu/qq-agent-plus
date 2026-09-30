<div align="center">

# QQ Agent Plus（个人分支）

面向 Linux 服务器的 QQ 群聊 Agent —— 在 [sakurawwwxh/qq-agent-plus](https://github.com/sakurawwwxh/qq-agent-plus) 基础上做的个人定制分支。

[![License](https://img.shields.io/badge/license-MIT-3da639.svg)](LICENSE)
[![Node](https://img.shields.io/badge/node-%E2%89%A522.13-339933?logo=nodedotjs&logoColor=white)](package.json)
[![OneBot](https://img.shields.io/badge/protocol-OneBot%20v11-12b7f5)](https://github.com/botuniverse/onebot-11)

</div>

---

## 参考项目 / 血缘

本仓库**不是从零开始的原创项目**，而是下面这条派生链上的个人分支。所有上游代码以 MIT 许可发布，
本仓库完整保留 `LICENSE`、`NOTICE.md` 与上游 Git 历史。

| 层级 | 仓库 | 说明 |
| --- | --- | --- |
| **直接上游** | [**sakurawwwxh/qq-agent-plus**](https://github.com/sakurawwwxh/qq-agent-plus) | 本仓库 fork 的基线，v0.7.4 |
| 上游的上游 | [carbonbromine/qq-agent](https://github.com/carbonbromine/qq-agent) | 二次改造 |
| 最初来源 | [K0nd1us/QQ-agent](https://github.com/K0nd1us/QQ-agent) | 原始项目，**Copyright (c) 2026 Kondius** |

> **版权**：上游代码版权归 Kondius 及各级上游作者所有（MIT）。
> 本仓库自基线 `8dca708` 之后的改动，版权归本仓库作者，同样以 MIT 发布。
> 详见 [`NOTICE.md`](NOTICE.md)。

**上游文档**（本仓库未改动，内容仍然适用）：
[`README.upstream.md`](README.upstream.md) ｜ [`README.en.md`](README.en.md) ｜ [`docs/`](docs/) ｜ [`AGENTS.md`](AGENTS.md)

---

## 本仓库相对上游的改动

### 一、新增功能

#### 撤回消息通知（RecallNotifier）

**QQ 撤回也删不掉。** 只要 bot 在消息被撤回之前已经把它存档，撤回后就会把原文私聊发给管理员。

- 监听 OneBot 的 `group_recall` / `friend_recall` 事件
- 从本地存档里取出**被撤回的原文**（文字 + 图片/语音/视频/QQ 表情）
- 以私聊形式推送给 `admin.ownerUin`，媒体作为后续消息补发
- 按 `chat_key + mid` 去重，同一条只通知一次（`recall_notifications` 表）
- 群聊会额外标注**撤回者** QQ 号

> 它**不阻止** QQ 撤回，只是在你本地已有记录的前提下发一份副本。
> 没被存档的消息（例如 bot 不在的会话、或早于存档时间的消息）无法恢复。

涉及文件：
`src/onebot/recall-notifier.js`（新增）、`src/core/store.js`（新增 `recall_notifications` 表与查询）、
`src/console/app.js`（事件接线）、`test/recall-notifier.test.mjs`（新增测试）。

### 二、问题修复

以下每条都对应一个实际踩到的失败模式：

| 文件 | 修复内容 |
| --- | --- |
| `src/core/providers.js` | **「测试模型」按钮误报超时**：原请求用 `max_tokens: 16`，推理类模型会把 16 个 token 全烧在思考上，撞上 20 秒硬编码超时 → 改成 `max_tokens: 256` 并简化提示词 |
| `src/tools/tools-core.js` | **`InvalidParameter.OversizeImage`（400，整次运行失败）**：取图上限原为 12 MiB，而视觉网关约 10 MB → 9.9~12 MiB 是盲区（本地放行、网关报错）。阈值改为 9 MiB，并接入 ffmpeg 降采样兜底 |
| `src/onebot/sticker-manager.js` | **「加进 QQ 收藏」100% 失败**：macOS 的 `com.apple.macl` 让普通进程写不进 QQ 沙箱容器，而 NapCat 的 `add_custom_face` 只认容器内路径。改为先用 NapCat 自己的 `download_file` 把图下进容器，再传容器内路径 |
| `src/onebot/stickers.js` | **自动收藏的表情永远用不上**：表情清单原文写着「发表情时优先挑〔QQ收藏表情〕」，把自动收藏（本地图库）的那批系统性压到最后；排序又按创建时间升序，新图永远轮不到。改为「两种都能用」+ 用 id 稳定哈希打散排序（保持缓存前缀不变） |
| `src/llm/llm.js` | **用量记账补全**：原生统计不覆盖所有调用路径，这里对每次 `chatCompletionWithRetry` 落一份全量记录（含 `purpose`、缓存命中数），便于排查消耗构成 |

---

## 快速开始

安装、部署、配置与协议端接入等完整说明，请直接看上游文档：

- [`README.upstream.md`](README.upstream.md) —— 完整中文文档（特性、部署、配置、FAQ）
- [`README.en.md`](README.en.md) —— English
- [`docs/LINUX.md`](docs/LINUX.md) —— Linux 部署细节
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) —— 架构说明

本分支的改动集中在上述几个文件，不改变原有的部署方式与配置格式。

---

## 许可与免责

- **许可**：MIT，见 [`LICENSE`](LICENSE)。上游版权归 Kondius 及各级上游作者；本分支改动归本仓库作者。
- **免责**：
  - 本软件连接你自己的 QQ 账号，自动化登录与使用方式可能违反相关服务条款，账号风险由使用者自行承担。
  - OneBot 协议端（NapCat / Lagrange / SnowLuma 等）是独立软件，请遵循其自身许可与条款。
  - 模型服务由第三方提供，费用、内容审核与可用性由服务商决定。
  - 本仓库**不包含任何密钥**，配置里的凭据由使用者自行填写与保管。
