# 配置示例（本分支新增项）

配置写在数据目录的 `config.json`（默认 `data/config.json`，含密钥，已被 Git 忽略）。
下面是本分支新增或调整过的键，其余键保持上游默认即可。

## api.thinking：思考档位（v0.7.4 起可按渠道/按供应商/按任务分设）

语义值：`on`（跟随服务商默认，也是默认值）/ `off` / `low` / `medium` / `high` / `max`。
程序按你填的 Base URL 识别渠道（内置 8 家预设），把档位翻译成该家真实参数；
预设没有的档位不会出现在控制台档位条上（例如 Command Code 没有"关"）。

```json
{
  "api": {
    "thinking": { "chat": "off", "default": "on" },
    "thinkingByService": {
      "api.deepseek.com": "low",
      "open.bigmodel.cn": { "chat": "off", "judge": "high", "write": "max", "default": "on" }
    },
    "thinkingParams": { "low": { "reasoning_effort": "low" } },
    "extraBody": { "reasoning": { "enabled": false } }
  }
}
```

- `thinking`：全局兜底。字符串 = 所有任务一档；对象 = 按任务分设，`chat`（聊天）/ `judge`
  （判断·总结：记忆整理、身份与关系评估、收表情）/ `write`（每日动态、空间互动文案）/ `default`（其他）。
- `thinkingByService`：**按供应商独立**，键是 Base URL 的主机名（含端口、小写）。某家配了自己的条，
  就覆盖全局 `thinking`；没配的家用全局。控制台 → 设置 → 高级选项里点档位条保存后自动写入。
- `thinkingParams`：**自定义档位映射**，给不在预设内的渠道用（键 = 档位名，值 = 该档实际发送的参数对象）。
  渠道识别不出来时会优先用它。
- `extraBody`：**额外请求参数**，以最高优先级合并进每次模型请求（含专用模型/兜底请求）；
  服务商文档里的怪参数都填这里。注意 `stream` 会被强制回非流式；`model` / `messages` / `tools`
  会整段替换对应字段。清空 JSON 时控制台走 `__replace__` 才能真删（deepMerge 只并集不删键）。
- 控制台「测试思考能力」按钮可按当前地址与 Key 实测：选"关掉"给"能不能关"的结论，
  选档位只报这一档能否通过；请求被 400 拒绝且错误提到 thinking/reasoning 参数时自动摘掉重试一次。

## api.fallback：兜底模型

```json
{
  "api": {
    "fallback": {
      "enabled": true,
      "baseUrl": "https://example.com/v1",
      "apiKey": "<另一个服务商的 key>",
      "model": "some-cheap-model"
    }
  }
}
```

主模型鉴权失败、额度不足或可重试错误耗尽时自动切换，日志里会写明原因；
主模型自身返回正常内容时不会触发（包括服务商审核拦截——那种情况会先用精简上下文重试一次）。

## api.vision / api.contextWindowTokens

`vision: true` 表示主模型能看图（收图后直接把图片交给模型）；上下文窗口按模型实际能力填写，
影响历史裁剪策略。

## proactive：主动开话题

```json
{
  "proactive": {
    "enabled": true,
    "checkIntervalMinMs": 9000000,
    "checkIntervalMaxMs": 12600000,
    "idleThresholdMs": 1800000,
    "probability": 0.55,
    "followUpEnabled": true,
    "selfWakeEnabled": true,
    "activeHours": {
      "start": null,
      "end": null,
      "windows": [{ "start": "09:00", "end": "12:00" }, { "start": "14:00", "end": "24:00" }]
    }
  }
}
```

- `enabled`：冷场时按概率主动开话题（控制台「设置 → 主动开话题」第一条），只管这一条链路。
- `followUpEnabled`：说完话没人接话时补一句（约 10 分钟后给一次机会）。与控制台同名的复选框对应。
- `selfWakeEnabled`：允许模型用 `schedule_wake` 给自己安排稍后的主动发言；关掉后该工具会从工具列表里摘掉。
- 这两项**独立于 `enabled`**，默认 `true`（与引入开关之前的行为一致）。想让它完全不主动开口，三个都要关；
  只想关掉"补话"或"自安排唤醒"的，各关各的。
- `activeHours.windows`：生效时段，窗口外不开口，也不消耗间隔；`24:00` 表示到当天结束（补话与自安排唤醒同样受这个窗口约束）。
- `idleThresholdMs`：群里安静这么久才考虑开口。
- `probability`：每次判定开口的概率。
- 冷场判定（`idleThresholdMs`）的时间会落盘：重启后不足一个间隔会跳过（避免"重启一次多一次判定"）。
  补话的 20 分钟节流**只在内存里**，重启后从零算，可能比预期多补一次（2026-09-26 审查发现文档与实现不一致）。
- 上面示例里的数值是**示例**、不是出厂默认：出厂 `idleThresholdMs` 1800000（30 分钟）、`maxIdleThresholdMs` 5400000、
  `probability` 0.25；照抄示例会改变行为。

## sticker：表情包

```json
{
  "sticker": {
    "enabled": true,
    "promptMaxStickers": 10,
    "collectEnabled": true,
    "autoCollect": true,
    "maxCollectPerHour": 10,
    "encourage": 3
  }
}
```

- `promptMaxStickers`：系统提示里常驻的可用表情条数（10/20/30/45/60 五档，在控制台
  「设置 → 聊天设置 → 所有模式 · 表情包」下拉选择）。清单一半按使用频次取常用的，一半留给没用过/很久没用的
  （发掉一张换下一张顶上来）；档位越大能选的范围越宽，代价是每轮提示词更长。库容量与此无关：
  收藏表情同步一律拉 500 条，全在库里；手改配置写成 1~60 以外的数会在保存/渲染时落到边界档。
- `autoCollect`：别人发来的表情包是否自动入库（同图只存一次，`maxCollectPerHour` 限频）。
- `encourage`：鼓励使用表情的力度（0-3），太频繁可调回 1-2。

## asr：语音转文字（可选，默认开启）

```json
{
  "asr": {
    "enabled": true,
    "provider": "openai",
    "baseUrl": "https://api.siliconflow.cn/v1",
    "model": "",
    "apiKey": "",
    "maxPerHour": 12,
    "language": "",
    "localBin": "",
    "localModel": ""
  }
}
```

- 作用：把消息里的语音、音频文件、视频音轨转成文字再交给聊天模型 —— 与模型是否多模态无关。
  控制台「设置 → 语音转文字」有同一组开关与字段。
- 控制台里「用哪种方式」只有两项：**API Key · 托管服务（推荐：更快、不用下模型，硅基流动/Groq 有免费额度）**
  或 **免费 · 本机安装的 Whisper（不联网、不要 Key，转写较慢）**；
  选了前者再由「服务预设」挑具体哪家（硅基流动 / Groq / 火山引擎 / 阿里云百炼 / 讯飞 / 腾讯云 / 百度 / 自定义自建）。
  模型**从服务商官网拉**（「获取模型列表」按钮打 `{baseUrl}/models`）—— 预设里写死模型名会过时，
  比如硅基流动新上免费模型时，列表跟着官网走才看得到。与「搜索服务」完全独立，不必是同一家、同一个账号：

  | provider | 说明 | 需要什么 |
  | --- | --- | --- |
  | `local` | 本机 **whisper.cpp**：不联网、不要 Key、无按量费用、音频不出机器 | 控制台「设置 → 语音转文字」点「安装本机转写」（推荐，带进度、装完自动生效）；装好后旁边有「完全卸载（删除模型与程序）」可释放约 500MB（删托管目录 `<数据目录>/asr` 与配置里指向它的路径；你另外装的 whisper.cpp/模型不会被碰）。或在服务器上跑 `node scripts/install-asr-local.mjs`，后者**要重启一次服务**（配置只在启动时读一次） |
  | `volc` | 火山引擎语音技术的**大模型录音识别（Seed-ASR）**，WebSocket，按量计费 | `apiKey`（语音技术控制台创建；也可用环境变量 `ASR_API_KEY`） |
  | `openai`（**默认**） | **任意 OpenAI 兼容**的转写服务：`POST {baseUrl}/audio/transcriptions`；默认预置硅基流动的地址（可一键换别家） | `apiKey` + `baseUrl`（到 `/v1` 那层）+ `model` |
  | `aliyun` | 阿里云百炼：走它的 OpenAI 兼容 chat 端点 + `input_audio`（默认模型 `qwen3-asr-flash`） | `apiKey`（百炼的 API Key）；地址/模型有默认值，可改 |
  | `baidu` | 百度短语音识别（标准版）：`POST vop.baidu.com/server_api`，JSON 带 base64 音频 | `apiKey`；老的「API Key + Secret Key」还要 `secretKey`（新的 `bce-v3/ALTAK-…` Key 只填 API Key） |
  | `tencent` | 腾讯云**一句话识别**：`POST asr.tencentcloudapi.com`，TC3-HMAC-SHA256 签名 | `secretId` + `secretKey`（访问密钥里的那对，不是 API Key）；地域默认 `ap-guangzhou` |
  | `iflytek` | 讯飞**语音听写**：`wss://iat-api.xfyun.cn/v2/iat`，签名 URL + WebSocket | `appId` + `apiKey` + `secretKey`(APISecret) 三个值 |

  **这四家的免费额度**（2026-09-26 查证，以各家官网当期政策为准）：
  讯飞语音听写约 **500 次/日**（新用户默认，另有一次性免费包）；百度短语音识别个人约 **5 万次**；
  腾讯云一句话识别约 **5000 次/月**；阿里云百炼新用户有免费额度（文生图/语音等按模型计）。

  ⚠️ 这四家都**不走 OpenAI 协议**，是各自的原生适配（腾讯的 TC3 签名与官方 SDK 交叉验证过、
  讯飞的分帧协议用本地假服务端跑通过；但**都没有用真实凭据端到端验证过**）。真机上若鉴权或参数不对，
  各家会回明确的错误码，这里会翻译成人话（例如"签名/鉴权没通过：核对 SecretId/SecretKey 是否填反"、
  "音频过长（超过 60 秒，需要分片）"），便于一轮定位。**超过 60 秒的音频会自动分片**（55 秒一片）
  后逐段转写再拼接。

- 本机转写的路径解析：`asr.localModel` / `asr.localBin` > 环境变量 `WHISPER_MODEL` / `WHISPER_BIN` >
  自动找（`<数据目录>/asr/`、仓库 `models/`、`~/.cache/whisper.cpp/`；二进制按 `whisper-cli` → `whisper-cpp` → `main`）。
  所以装完脚本后**什么都不用填**，控制台里也会显示"当前会自动用哪两个路径"。
- **免费选项**（都走 `openai` 那支；控制台里选「服务预设」会把**地址**填好，**模型**要点「获取模型列表」从官网拉
  —— 预设里写死模型名会过时；你只需粘一个该服务的 Key。
  免费政策是会变的，下面的事实核查于 2026-09-26，用之前建议看一眼各家价目表）：

  | 服务 | baseUrl | model | 免费情况 |
  | --- | --- | --- | --- |
  | **硅基流动**（推荐，国内可直连） | `https://api.siliconflow.cn/v1` | `FunAudioLLM/SenseVoiceSmall` | 官方价目表标**免费**；单文件 ≤1 小时 / ≤50MB |
  | Groq | `https://api.groq.com/openai/v1` | `whisper-large-v3-turbo` | 有免费额度（约每分钟 20 次、每天 2000 次、每天 8 小时音频）；单文件 25MB；国内可否直连未确认 |
  | 自建 | 你自己的网关 | 自定 | 免费（前提是你有机器），faster-whisper 等 |

  **没有任何托管服务能做到"零注册零 Key"** —— 想完全不注册账号，只有 `local`（本机 whisper.cpp）：
  一次装好后永久免费、离线可用，但 CPU 转写大约 1.5~2 倍实时（2 核机器上 30 秒语音约 15~25 秒），
  长音频会很慢；中文建议 `ggml-small.bin`（约 466MB）起步。
- 没配齐时这项**完全不生效**：工具不会注入给模型、不会产生任何调用与费用，提示词照旧说"听不了语音"。
- 凭据与"存它时的服务"绑定：`apiKeyProvider`、`secretIdProvider`、`secretKeyProvider`，OpenAI 兼容服务
  再加一层 `apiKeyHost`（主机名）。换服务、或换到另一个主机（硅基流动 ↔ Groq ↔ OpenAI 都算 `openai` 这一家，
  只看 provider 分不出来）之后，旧凭据**不会被发到新地址**，控制台会提示重新填一次。环境变量 `ASR_API_KEY`
  不受此限（它是部署级的单一值）。升级上来的老配置在读盘时自动补记归属，原来能用的照常能用。
- `language` 可选（`zh` / `en`…），留空由服务自己判；`maxPerHour` 是每小时的硬闸门（跨会话共享，默认 12，
  非正数按 12 处理）。**按"转写一条消息"计**：超过 55 秒的音频会拆成多段请求，服务商那边的额度是按段扣的
  —— 一条 13 分钟的语音约等于 15 次上游调用（2026-09-26 审查标注口径）。
- `local` 与 `iflytek` 要按音频真实时长逐帧推流：一次运行的总时限（默认 180 秒、上限 240 秒）里转不完长音频，
  超过约 150 秒（本机约 75 秒）会**提前**给一句可照做的提示，而不是等两分钟才超时。
- 需要服务器上有 `ffmpeg`（转码用）；单条音频上限 15 分钟，超长会直接报错不转写。
- **视频给的是"画面 + 声音"两样**：画面由 get_message_images 抽 4 帧拼成 2×2 帧条（JPEG）给视觉模型，
  声音由 get_message_audio 转写音轨；两者独立，模型可以只取其一。协议端若只给文件名、没给可下载 URL
  （NapCat 在视频只存在本地缓存时如此），工具会明说拿不到地址并建议用"发送文件"重发，而不是报一句"URL 无效"。
- **QQ 语音是 SILK 格式**（文件名写着 `.amr`、服务端还回 `content-type: audio/mp3`，但字节是
  `.#!SILK_V3`）—— ffmpeg **不支持** SILK，所以这条链路不走 ffmpeg：先用 `silk-wasm`（WASM，
  随依赖安装）在本地解成 16k 单声道 PCM，再交给所选服务转写。协议端的 `get_record` 不会替你转码
  （实测 NapCat：传 `out_format=mp3/wav` 仍返回同一个原始 URL），所以这一步必须自己做。

## tts：语音回复（可选，默认关闭）

```json
{
  "tts": {
    "enabled": true,
    "provider": "openai",
    "baseUrl": "https://api.siliconflow.cn/v1",
    "apiKey": "<语音合成的 Key>",
    "model": "FunAudioLLM/CosyVoice2-0.5B",
    "voice": "FunAudioLLM/CosyVoice2-0.5B:anna",
    "speed": 1.1,
    "gain": 0
  }
}
```

- **四种适配器**（按 `provider` 分发，`src/llm/tts.js`）：
  `openai`（兼容 `POST {baseUrl}/audio/speech`，硅基流动/OpenAI/自建，**已实测**）、
  `volc`（火山引擎语音合成 **v1** HTTP，需要数字 `appId` + `apiKey`=Access Token，`cluster` 固定 `volcano_tts`，
  音色如 `BV001_streaming`；老接口，账号没开通 v1 服务时会固定报 3001）、
  `doubao`（火山引擎**豆包大模型语音合成 2.0**，`https://openspeech.bytedance.com/api/v3/tts/unidirectional`，
  只填 `apiKey`（控制台密钥，走 `X-Api-Key`）与 `voice`，`resourceId` 默认 `seed-tts-2.0`；
  **2026-09-28 用真实账号实测**：音频/语速/音量/格式/错误码都核过，自然度明显高于 v1 与硅基流动）、
  `minimax`（T2A v2，需要 `apiKey` + `groupId`，音色如 `female-shaonv`，音频是 hex 编码、适配器已处理）。
  volc / minimax 按官方文档实现但机器上没凭据、未实测：填好凭据点控制台「试听」即可验证，报错会把人话原因（含火山错误码含义）带出来。

豆包 2.0 的写法（当前最推荐的音质）：

```json
{
  "tts": {
    "enabled": true,
    "provider": "doubao",
    "baseUrl": "https://openspeech.bytedance.com/api/v3/tts/unidirectional",
    "keys": { "doubao": "<语音技术控制台里的密钥>" },
    "resourceId": "seed-tts-2.0",
    "voice": "zh_female_vv_uranus_bigtts",
    "speed": 1.1,
    "gain": 2
  }
}
```

- 与 `asr` 对称但**默认关**。
- **服务预设**：控制台「设置 → 语音回复」有预设下拉（硅基流动 / OpenAI 官方 / 火山 v1 / 豆包 2.0 / MiniMax / 自定义），
  选中会把地址、资源 ID 填好，并给出这家的常用模型与音色候选；OpenAI 兼容那几家还能点「获取模型列表」从官网拉全量。
  音色没有可查接口（硅基流动与 OpenAI 都实测 404），只能内置——表在 `src/llm/tts-presets.js`。
  火山/豆包/MiniMax **没有模型列表接口**，界面上会直接说明该填什么（不再显示"共 0 个"）。
- **火山的两套凭据不一样，别混**：v1 要「应用管理」里的数字 AppID + Access Token + `cluster=volcano_tts`；
  豆包 2.0 只要控制台密钥（`X-Api-Key`），**不需要 AppID / Cluster**，但要多填一个 `resourceId`。
  常见填错：把资源 ID（`seed-tts-2.0`）填进 AppID、把音色名（`zh_*_uranus_bigtts` / `ICL_uranus_*`）填进 Cluster ——
  控制台会当场提示，服务端报错也会说明（`3001` = AppID/Token 不对；`45000010` = Key 不对；`45000030` = 没开通该资源；
  `55000000` = 音色与资源 ID 不匹配）。
- 豆包 2.0 的音色表 = **火山官方 2.0 全量清单（102 个，按控制台分类分组）**，控制台「候选音色」里的分类与火山控制台的音色列表一一对应；
  2026-09-28 在真实账号上把这 102 个逐个打过一遍：中文 99 个全部可用，3 个英文音色该账号未开通（会用 `resource not granted` 说明，别的账号可能已开通）。
  想用自己克隆的音色（形如 `ICL_uranus_zh_female_xxx_tob`）直接手填进「音色」即可 —— 界面不会自动替换不在表里的音色。
- `voice`：硅基流动是「模型id:音色」（`…:anna / bella / claire / diana / benjamin / alex / charles / david`），OpenAI 官方是裸音色名（`alloy / nova / …`），火山系是音色 ID。
- `speed`（0.25~4）与 `gain`（-10~10 dB）：本机实测**真实生效**（同一句 0.7→8.1s、1.3→4.7s）；聊天语速 1.05~1.15 更活，觉得闷可以把 gain 提 2~4。
- 平淡的另一半在**文本**：合成内容要写成口语（语气词 + 「——」「……」「？」带停顿），工具描述里已这样教模型。
- 开启后模型多一个 `send_voice` 工具：短句 1~3 句、≤120 字最自然；提示词只会在开启时才教它用。
- 协议端需要支持 `record` 消息段；不支持时发送会失败并提示改用文字（不会反复重试）。
- 与 `asr` 的 Key 分开配置（听和说是两个服务是常态）。

## groupDigest：群日报（可选，默认关闭）

```json
{
  "groupDigest": {
    "enabled": true,
    "time": "09:30",
    "chats": ["group:123456"],
    "maxChars": 300
  }
}
```

- 每天到点把「过去 24 小时这个群聊了什么」汇总成一条，用机器人的人设口吻发到群里（只发白名单里的群）。
- 少于 5 条消息的群当天跳过；生成走 `purpose:'write'`，吃对应思考档位与省 Token 上限。
- 手动触发/查看状态：`POST /api/group-digest/run`、`GET /api/group-digest/status`。

## groupGame：群游戏（实验性，默认关闭）

```json
{
  "groupGame": {
    "enabled": true,
    "chats": ["group:123456"],
    "allowPrivateInvite": true,
    "dailyLimitPerChat": 6,
    "maxDurationMin": 60
  }
}
```

- `maxDurationMin` 是单局时长上限，**与游戏自身的上限取较小值**（数字炸弹 20 分钟、谁是卧底 45 分钟）；
  设得比它俩大不会延长。到点引擎自动结算。
- 开局名单缺省=最近发过言的群友（上限 `maxPlayers`）。模型可以用 `group_game` 的 `players` 参数
  锁定名单，传 QQ 号或**群名片**都行（名片要一字不差，且必须最近发过言——这是防提示注入把词发给任意 QQ 的硬门）。

- 主持人由模型担任（氛围/解说），**轮次、计票与判定由状态机负责**（`src/features/group-game.js`）：
  数字炸弹（1~100 猜数，谁踩中谁输）、谁是卧底（4~10 人，词只走私聊，公开摘要不含身份）。
- 白名单制、每群同时一局、每天每群有开局上限；进行中的局落盘 `data/games.json`，重启可恢复，超时自动收尾。
- `allowPrivateInvite`：卧底发词必须开（只发给报名者、每人每局一条、失败不重试）；默认关。
- 控制台入口：设置 → 实验功能 → 「群游戏」。开局：群里说「来个数字炸弹 / 谁是卧底」。

## pacing：自主节奏（实验性，默认关闭）

```json
{
  "pacing": {
    "enabled": false,
    "scope": "group",
    "instantOnMention": true,
    "defaultWakeMinutes": 20,
    "minWakeMinutes": 5,
    "maxSilenceMinutes": 45
  }
}
```

开启后消息不再即时触发，模型按自己安排的节奏醒来统一处理；私聊永远即时，
`instantOnMention` 保证被 @ 时立刻响应。适合"不想每条都秒回"的场景，建议先小范围试。

## tokenSaver：省 Token 模式

```json
{
  "tokenSaver": { "mode": "balanced" }
}
```

- `mode`：`off`（默认）/ `balanced`（省）/ `aggressive`（很省）。控制台在「设置 -> 省 Token」，带"你的设置 / 当前生效"对照表。
- 语义：**只夹上限，不改写** `store.atCount` / `api.maxRounds` 这些用户值；关掉立刻回到原设置（`off` 与升级前一致；手改成负数这类坏值时会按默认值处理，不再原样传下去）。
- 被夹的项：上下文档位条数（被艾特/关键词/随机/全响应）、单次运行工具轮数与累计 Token、
  会话交接与全局印象注入的字符上限、系统提示里的表情清单条数。
- 有一处例外：每日动态（说说）另有一条提示词组装路径，它自己取交接与印象，不吃上面那两个字符上限
  （那条路径有自己的 `maxPromptChars` 预算兜底）。
- 改不动的部分：每次模型调用的固定底（系统提示 + 工具定义）约 1.2 万-1.5 万 token。

## 安全提示

- `config.json` 含 API Key、控制台 Token、OneBot Token，不要提交、不要外发；
- 控制台 Token 可在 `设置 -> 系统 -> 控制台安全` 中轮换；
- 换模型时先确认 `vision`、`contextWindowTokens`、`maxRunTokens` 三项与模型能力一致，
  否则会出现"图片被当成文本"或"上下文被过早裁剪"。
