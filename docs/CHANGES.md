# 改动清单（CHANGES）

本项目的开发起点是 revision `8dca708`（衍生关系见 NOTICE），之后叠加的改动集中在五块：**对话行为、发送链路健壮性、贴纸系统、主动发言、运维**。

这些改动当初是逐条以"补丁脚本"的形式打到部署机上的（脚本头部注释记录了当时的复现现象、失败模式与实测数字），后来沉淀进源码。下表"原补丁脚本"列即对应脚本名；没有对应脚本的是直接改源码/配置，标为「本仓库新增」。源码侧 diff 约 1.2 万行（含整文件重排），UI 侧只有两个小改动。

## 总表

| 功能 | 涉及文件 | 一句话说明 | 原补丁脚本 |
| --- | --- | --- | --- |
| 分条/多气泡发言 | `src/llm/prompt.js` | 一轮想说的分 2-3 条短句发，禁止用空格把两句连成一条 | `apply-chat-bubbles-patch.sh` |
| 提示词调优 | `src/llm/prompt.js`、`src/llm/qzone-interaction-prompt.js` | 明确"被点名默认要回、没被点名可以不回"等边界；表情/图库用法同步更新 | `apply-prompt-tune.sh` |
| 聊天关思考 | `src/llm/llm.js`、`src/core/orchestrator.js` | 按用途传 thinking 开关：聊天不带思考，判断/写作类保留 | `apply-chat-thinking-off.sh` |
| 看图先读情绪 | `src/llm/prompt.js`、`src/tools/tools-core.js` | 禁止描述画面，要求先给情绪定性再回话（v2 进一步收紧并给正反例） | `apply-vision-emotion.sh`、`apply-vision-emotion-v2.sh` |
| 自安排唤醒 `schedule_wake` | `src/core/orchestrator.js`、`src/tools/tools-core.js`、`src/llm/prompt.js`、`src/console/app.js` | 模型可以给自己安排一次"稍后主动开口" | `apply-schedule-wake-patch.sh` |
| 补话（说了没人接） | `src/core/orchestrator.js`、`src/llm/prompt.js` | 发过言、没人接、在发言时段内时，约 10 分钟后给一次补话机会 | `apply-followup-nudge.sh` |
| 提示词收尾自检 / 发言唯一通道 / 多气泡鼓励 / 闲聊带自己 / 表情清单常驻 | `src/llm/prompt.js` | 一批提示词层面的行为约束 | 本仓库新增（直接改源码） |
| 消息 id 归一化 | `src/tools/tools-core.js`、`src/core/store.js` | 把 `#123` 归一成纯数字 id；helper 与调用点绑定插入，避免"调用点有、定义没有" | `apply-message-id-normalize.sh` |
| 发送网络级重试 | `src/onebot/sender.js` | `fetch failed` 重试一次；限频等非网络错误不重试 | `apply-send-retry.sh` |
| 内联工具调用兜底 | `src/tools/inline-tools.js`（新增）、`src/core/orchestrator.js`、`src/features/daily-moments.js`、`src/identity-pilot*.js`、`src/pilots/relationship-pilot.js`、`src/features/qzone-interactions.js`、`src/onebot/sticker-manager.js` | 模型把 tool call 写成文本（Hermes XML / 裸 JSON）时也能取到决定 | `apply-inline-toolcall-fallback.sh` |
| 发 QQ 系统表情 `send_face` | `src/onebot/onebot.js`、`src/onebot/sender.js`、`src/tools/tools-core.js`、`src/llm/prompt.js` | 按中文名发系统表情，支持"文字+表情"同一条混排 | `apply-send-face-patch.sh` |
| 系统表情标签 | `src/onebot/onebot.js`、`src/llm/prompt.js` | 来信里的系统表情标成 `[QQ表情N 名字]`，与表情库 id 区分 | `apply-face-label-clarity.sh` |
| 启动/重连补课 | `src/console/app.js` | 断线或重启期间丢的消息，从协议端拉最近历史补齐（按 mid 去重） | `apply-chat-catchup.sh` |
| 启动自检 + 静态扫描 | `src/ops.js`（`scan` 子命令） | 扫"调用点有、定义没有"的函数名，只记日志、不阻断启动 | 本仓库新增（由消息 id 归一化事故催生） |
| 已理解过的图直接回备注 | `src/tools/tools-core.js` | 库内图的备注直接复用，省一次视觉调用 | `apply-known-sticker-hint.sh` |
| 表情包自动收藏 | `src/onebot/sticker-manager.js`、`src/onebot/stickers.js`、`src/console/app.js`、`src/core/config-legacy.js` | 看别人发的图判断值不值得收；判断异步、不阻塞主流程 | `apply-sticker-autocollect.sh` |
| 收藏判断健壮性 | `src/onebot/sticker-manager.js` | 认内联提交；`max_tokens` 200 → 600；判定尝试 2 → 3 次 | `apply-sticker-judge-robust.sh`、`apply-judge-retry3.sh` |
| 优先 QQ 收藏表情 | `src/onebot/sticker-manager.js` | 值得收时优先加进 QQ 收藏（链接稳定），失败退回本地库 | `apply-sticker-qq-favorites.sh` |
| 表情同步防清空 | `src/onebot/stickers.js` | QQ 收藏列表为空/失败时不剪枝，避免本地库（含备注）被清空 | `apply-sticker-sync-guard.sh` |
| 找不到表情时的兜底 | `src/onebot/stickers.js`、`src/tools/tools-core.js` | 报错里带上有效 id；`findSticker` 加唯一命中的模糊匹配；提示直接用备注名选图 | `apply-sticker-lookup-help.sh` |
| 表情备注上限 | `src/onebot/sticker-manager.js` | 16 → 24 字 | `apply-sticker-note-length.sh` |
| `[表情包]` 标签与收藏规则收紧 | `src/console/app.js`、`src/llm/prompt.js`、`src/tools/tools-core.js`、`src/onebot/stickers.js`、`src/onebot/sticker-manager.js` | 表情包消息单独标注；只收真表情包，生活照/自拍不收 | `apply-sticker-label-and-rule.sh` |
| 提示词告知可攒表情 | `src/llm/prompt.js` | 工具一直有，只是没告诉模型 | `apply-sticker-collect-prompt.sh` |
| 主动开话题节奏 | `src/core/orchestrator.js` | 间隔 2.5-3.5 小时；"没有安静的群"不算消耗本轮（45 分钟后再看） | `apply-proactive-cadence.sh` |
| 主动判定间隔守卫 | `src/core/orchestrator.js` | "上次判定时间"落盘，重启后不足一个间隔就跳过 | `apply-proactive-interval-guard.sh` |
| 主动行为可观测 | `src/core/orchestrator.js` | 跳过原因、真正开话题都记日志；每次 tick 最多一行 | `apply-proactive-observability.sh` |
| 主动发言活跃时段 | `src/core/orchestrator.js` | 支持多个时间窗口（如 9-12 与 14-24）；窗口外不开口，也不浪费间隔 | `apply-proactive-quiet-hours.sh` |
| 空间互动专属活跃时段 | `src/features/qzone-interactions.js` | 动态互动单独设时段，不影响聊天回复 | `apply-qzone-active-hours.sh` |
| 空间互动失败退避 | `src/features/qzone-interactions.js` | 接口连续失败时指数退避，避免失败风暴 | `apply-qzone-fail-backoff.sh` |
| 空间互动抓取容错 | `src/features/qzone-interactions.js`、`ui/app.js` | 好友动态抓取失败（腾讯侧 `network busy` / 使用人数过多）先重试一次，仍失败也不再让整轮失败：评论检查与未读积压照跑；连续第 3 次才上报异常通知 | 本仓库新增 |
| 每日说说查重容错 | `src/features/daily-moments.js` | 空间列表读不到时跳过查重，不阻断发布 | `apply-moment-dedup-fix.sh` |
| 控制台端口探测修复 | `src/console/integrations.js` | 上游写死旧端口 15099/16081，与 Linux 全栈的 5099/6081 不一致导致误报"不可达" | `reapply-console-port-fix.sh` |
| 控制台自动登录 | `ui/app.js`、`src/console/app.js` | 地址栏带 `?token=` 免输令牌（成功后清掉 URL 明文）；登录 cookie 改 30 天 | `apply-autologin-patch.sh` |
| 会话列表轮询校准 | `ui/app.js` | 配置就绪后重新校准轮询间隔，消除每 4 秒重建列表的闪烁 | 本仓库新增（见 UI diff） |
| 只在源码变化时重启 | 部署脚本 `restart-if-changed.sh` | 每小时更新链不再无条件重启、打断正进行的对话 | `restart-if-changed.sh` |
| 运维工具集 | `src/ops.js`（单入口）、`docs/OPS.md` | 主机/服务自检、备份、进程看门狗、发送/登录线上验证、表情名导出、非交互部署、SSH 隧道、systemd 定时器安装 | 本仓库新增 |
| 本地回归测试 | `test/local/` | 定点验证发送重试、退避、内联兜底、贴纸查找 | 本仓库新增 |
| 省 Token 模式 | `src/core/token-saver.js`（新增）、`src/core/config-legacy.js`、`src/llm/prompt.js`、`src/core/orchestrator.js`、`src/memory/memory-global.js`、`src/features/daily-moments.js`、`src/features/qzone-interactions.js`、`ui/app.js`、`src/console/app.js` | 「设置 -> 省 Token」三档，只给上下文档位条数、单次运行轮数与预算、交接/印象注入字符数、表情清单条数**夹上限**，不改写用户设置；关掉即恢复原样 | 本仓库新增 |
| 关闭上游调试探针 | `src/*.js`、`ui/*.js` | 上游作者留在源码里的调试上报（指向其开发机私网地址）全部关掉 | `apply-disable-upstream-debug.sh` |

## 0. 思考控制与表情匹配（v0.7.4 起）

- **思考控制（按渠道翻译档位、每家独立、可按任务分设）**：`src/core/provider-presets.js`（新增）、
  `src/llm/llm.js`、`src/core/providers.js`、`src/console/app.js`、`ui/app.js`、`src/core/config-legacy.js`。
  失败模式：思考原先只有"开/关"两态、只写一种参数形态，而各家的开关根本不是同一个参数 —— Command Code 认
  `reasoning_effort`（且没有"关"档，`thinking` 字段被网关静默吞掉）、DeepSeek 官方认 `thinking.type=disabled`、
  通义认 `enable_thinking` + `thinking_budget`、OpenAI 用 `none` 当"关"……写死的形态在别的渠道上要么静默失效、
  要么被 400 拒绝。现行做法：语义层只表达意图（跟随服务商默认 / 关 / 低 / 中 / 高 / 最高），由渠道预设按
  baseUrl 主机名翻译成该家真实形态；预设只列官方文档给出的档位（智谱 GLM-5.3/4.7/4.5V 强制思考、Command Code
  无 off 档 → 都不列"关"）。三处细分：**按供应商独立**（`api.thinkingByService[host]` 覆盖全局 `api.thinking`）、
  **按任务分设**（`{chat, judge, write, default}`：聊天 / 判断·总结 / 写作 / 其他，例如"聊天关、判断开"，
  带 `purpose` 的调用点在编排器、表情判断、身份/关系试航、空间互动等处）、**两条逃生口**（`extraBody` 直接并进
  请求体、优先级最高；`thinkingParams` 给不在预设内的渠道自定义档位映射；控制台里清空 JSON 要 `__replace__` 才真删）。
  能力实测（控制台按钮）只报实测到的事实：选"关掉"给"能不能关"的结论，选档位只报这一档能否通过、不含关闭结论；
  请求被 400 拒绝且错误提到 thinking/reasoning 参数时自动摘掉该参数重试一次。默认值不变（`thinking: 'on'`、
  跟随服务商默认），升级不影响既有实例。
- **表情匹配：send_sticker 报「找不到表情」（Issue #17）**：`src/onebot/stickers.js`。
  失败模式：`findSticker` 把标签与备注同池做双向包含、再要求唯一命中，别的表情的短标签擦边命中会把真正的备注
  命中一起否决 → 库里有也返回 null；模型把清单里「备注 [标签]（用过N次）（stickerId：…）」整行抄回来时同样匹配不上。
  现行做法：分级匹配（备注优先，只在同一级内要求唯一）id/md5/url → 备注精确相等 → 备注包含查询 → 查询包含备注 →
  标签；展示性修饰做成"由长到短"的形态阶梯、从最完整形态开始试（备注自身以括号结尾如「裂开（崩溃）」时不会被
  剥短形态劫持到别的条目）；能从「（stickerId：xxx）」里抠出 id 兜底。生产库 37 条清单行 + 截断行实测 66/66 命中。
- **工具报错写进 journal 并统一脱敏**：`src/core/orchestrator.js`、`src/core/redact.js`（新增）。
  失败模式：工具失败只进控制台异常面板，journal 里查不到，排查时容易漏（Issue #17 的补充建议）。现行做法：
  同一判定口径下补一行 `[tool] … 出错：…`，文本走统一脱敏（与 incident-pilot 入库同一套规则），并把
  `access_token` / `api_key` 这类带下划线前缀的参数名补进规则（旧规则只认 `?token=` / `?key=`，会漏掉本项目
  OneBot 实际写在查询串上的 `access_token`）。

## 1. 引用、记忆与人设（v0.7.3 起）

- **引用块带被引用那条的消息 id**：`src/core/util.js`（`formatQuoteRef` / `quotePrefixFor` / `textWithQuote`）、
  `src/onebot/onebot.js`、`src/llm/prompt.js`、`src/tools/tools-core.js`、`src/console/app.js`。
  失败模式：引用块里只有说话人名和原文、没有消息 id，且机器人自己被引用时显示的是群名片名（历史行里却是「我」）——
  模型定位不到被引用那条、也认不出那是自己说的话，于是对"谁在回谁、哪条在前"给出错误回答（Issue #16）。
  现行做法：统一渲染成 `[引用#102·说话人：原文]`，引用自己的消息标「我」；实时消息、历史记录、翻页工具、
  消息详情、控制台消息接口共用同一套渲染。形态里不放空白（`sanitizeUserText` 会折叠方括号内空白，
  老的前缀判定 `startsWith('[引用 ')` 因此在真实存档上从未命中，历史行会重复贴一次引用、「引用」标签也一直没生效）。
- **过去状态的边界说明与翻页补偿**：`src/llm/prompt.js`、`src/tools/tools-core.js`。
  失败模式：历史只带最近 N 条却没有任何说明，模型把"没看到"当成"不存在"；`pastStateCount` 少算触发批，
  `get_recent_messages` 翻页开头会重复触发批那几条。现行做法：写明条数、`#消息id` 通常随时间递增可核对先后
  （被补课的旧消息/跳号时以行序为准）、更早的用工具往前翻；补偿按"触发批 + 过去状态"计。
  「有历史但这次没带」不再误报"这是你第一次参与这个会话"。
- **emoji 被切成半个导致整次模型请求 400**：`src/core/util.js`（`safeSlice` / `stripLoneSurrogates`）、
  `src/llm/llm.js`（请求前兜底清理）、`src/core/orchestrator.js`、`src/llm/prompt.js`、`src/tools/tools-core.js`、
  `src/console/app.js`。失败模式：记忆"新建印象"把该群友的发言按 200 字符切片，正好切出半个 emoji（孤立代理项），
  模型网关把整次请求判成 400 Bad Request —— 该群友永远建不出印象，控制台只显示"1 位失败（已保留原印象）"。
  实测：原样请求 400，剥掉孤立代理项后同一请求 200 并成功产出印象。现行做法：截断一律走 `safeSlice`（不切断代理对），
  并在请求出口统一清掉孤立代理项。
- **人物记忆的"发现新人"门槛改为控制台可设**：`ui/app.js`、`src/core/orchestrator.js`。
  失败模式：自动整理只在"最近 2000 条里发言 ≥ discoverMinMessages（默认 20 条）"的零印象群友里挑人，
  门槛高于群里多数人的实际活跃度时，新记忆永远不会产生（实测某群：已有印象者 225/127/45 条，
  其余人 16/11/8 条全部够不到门槛）。现行做法：控制台「记忆整理」可设「发现新人的最少发言条数」与
  「单次最多发现几人」，默认值不变。
- **换人设不再残留上一张卡**：`src/llm/prompt.js`、`src/console/app.js`、`ui/app.js`、`roles/*.md`。
  失败模式：换卡 26 小时后仍在用旧卡的口癖（实测：群 433397830 最近 30 条里它自己 8 条带"喵"，群友 0 条，
  而当前卡明文写着"不要：可以哦，喵～"），根因是"自我锚定"——自己上一条的口气、各群的交接/checkpoint、
  表情库里的旧人设道具都在提示词里。现行做法：`persona.changedAt` 由控制台在内容真变时打点，
  24 小时内【过去状态】等段落自动加"旧记录不作数"提示；新增按钮「换人设后清空交接」
  （`POST /api/persona/reset-handoffs`，需确认；不动聊天记录与人物印象）；平台提示词里写死的默认人设元素全部中性化。
- **表情库区分"QQ 收藏表情"与"本地图库"**：`src/onebot/sticker-manager.js`、`src/onebot/stickers.js`、
  `src/tools/tools-core.js`、`src/console/app.js`。失败模式：库里的收藏项存的是消息图片的临时链接，
  发出去是普通图片、链接过期后是坏图；`collect_sticker` 直接进本地库、不判断"是不是表情包"。现行做法：
  收藏即落盘（`sticker-assets/`），清单标出来源与发送形态（〔QQ收藏表情〕/〔本地图库·发出去是图片〕），
  发送前探活、失效不发并给出可照做的提示；QQ 收藏夹上限 500（非会员）因此本地库保留。

## 2. 语音转写与视频（v0.7.2 起）

- **多供应商语音转写**：`src/llm/asr-openai.js`、`asr-local.js`（本机 whisper.cpp）、`src/llm/seed-asr.js`（火山 Seed-ASR）、
  `asr-dashscope.js`（阿里云百炼）、`asr-baidu.js`、`asr-tencent.js`（TC3 签名）、`asr-iflytek.js`（签名 WSS 分帧）+
  `src/tools/audio-transcribe.js`（路由/分片/配额/非语音提示）。四家国内云的短语音接口单次 ≤60 秒，
  统一按 55 秒无损分片再拼接；OpenAI 兼容服务超过 20MB 才切片（25MB 上传上限）。失败模式：长音频被服务端整条拒掉、
  分片中途失败丢掉已计费的前几片、纯音乐/音效被 ASR"编"出一段像模像样的假文本 —— 分别用分片、进度写进错误、
  停顿比例判据（≥3 秒且近静音 <5%）处理，命中时把"别把上面的文字当作事实讲"一并交给模型。
- **QQ 语音是 SILK**：`src/llm/silk.js`。真实字节是 `.#!SILK_V3`（文件名写着 `.amr`、CDN 回 `content-type: audio/mp3`，
  都不可信），ffmpeg 没有 SILK 解码器 → 一律 "Invalid data found"。协议端的 `get_record` 不会转码
  （实测传 `out_format=mp3/wav` 仍返回同一个 CDN URL），因此用 `silk-wasm`（WASM，延迟加载）在本地解成
  16k 单声道 PCM。协议端只给文件名、没给 URL 时，语音走 `get_record`、群/私聊文件走
  `get_group_file_url` / `get_private_file_url` 换地址。
- **视频"看画面 + 听声音"**：`src/onebot/onebot.js`（video 段与视频类文件段各留 audio/video 两条）、
  `src/tools/image-downsample.js`（`convertVideoToFrameStrip`：ffprobe 取时长 → 均匀抽 4 帧拼 2×2 JPEG）、
  `src/tools/tools-core.js`（`get_message_images` 按 kind 分流）。失败模式：只采音轨时模型会回"视频只能听声音"
  （用户实测反馈），画面根本没进过模型的眼睛。

## 3. 对话行为

- **分条发言（多气泡）**：`src/llm/prompt.js`。失败形态有两种：一是"把想说的全塞进一条长消息"，二是"用空格把两句连成一条"。补丁注释记录，v1 之前实测 90% 的情况只发一条；v2 在尾部加了"别把一轮压成一句点评"，并明确"一轮常见 2-3 条短句、单条多数 ≤30 字、别一口气刷 4 条以上"。配套的 `humanRhythm` / 主体性文本属于上游自带内容，未通过脚本改动。
- **提示词调优**：`src/llm/prompt.js`、`src/llm/qzone-interaction-prompt.js`。把"被 @ 或直接提问时优先判断是否需要回应"改成"被 @、点名或直接提问时默认要回一句（可以短、可以敷衍、可以怼回去），只有明显与你无关、对方 @ 别人、或纯刷屏误 @ 时才不回"（v0.6.3 起把其中的"可以怼回去"进一步软化为"也可以就回一句不痛不痒的"）；同时统一了"图库可以自己攒"的用法说明。
- **聊天关思考**：`src/llm/llm.js`、`src/core/orchestrator.js`。聊天主调用传 `purpose:'chat'`，不携带 thinking 字段；判断/写作类调用不传，走 `default:'on'`。配置 `api.thinking = {chat:'off', default:'on'}`；脚本幂等，写配置前才停服务。
- **看图先读情绪**：`src/llm/prompt.js`、`src/tools/tools-core.js`。模型看表情包/图片时容易去"描述画面"；改成先定性情绪再回话，v2 进一步收紧并给出正反例。顺手修了一个缺失：看库内表情时只给了 `desc`，没给模型自己写的 `localNote`。

## 4. 发送链路健壮性

- **消息 id 归一化**：`src/tools/tools-core.js`、`src/core/store.js`。模型常把提示词里的 `#123` 连 `#` 一起传回来，而 OneBot 只认纯数字 id。关键教训：`tools-core.js` 用到的 `normalizeMid` 必须在同一个文件里定义（`store.js` 里那份是模块私有、没有 export），早先只替换调用点没插 helper，结果每次 `send_message` / `send_sticker` / `send_face` 都抛 `normalizeMid is not defined`，机器人一个字都发不出去。所以脚本把"插 helper"和"替换调用点"绑在一起，并且在最后自检两者必须同时存在。
- **发送网络级重试**：`src/onebot/sender.js`。协议端重启或连接被掐时会抛 `fetch failed`，原来直接丢消息（用户视角是"它没回我"）；网络层错误重试一次即可救回，限频/参数类错误不重试（重试也没用）。回归用例见 `test/local/test-sender-retry.mjs`。
- **内联工具调用兜底**：新增 `src/tools/inline-tools.js`，并接入编排器之外的所有判断类模块（空间互动 / 每日说说 / 身份评估 / 关系评估 / 表情收藏判断）。失败模式：模型有时不返回原生 `tool_calls`，而是写成 `<tool_call><function=...>` 文本或带 `name` 的 JSON，这些模块只认原生结构 → 决定被当成"没提交"丢掉（线上出现过"关系评估模型未提交唯一的 submit_relationship_events 结果"、以及"模型两次都没提交决定，这次跳过"）。做法是把编排器里的解析器抽成共享模块，新增 `resolveToolCalls(message)` 统一成 OpenAI 结构，其余代码照旧读 `call.function.name / arguments`。
- **启动/重连补课**：`src/console/app.js`。服务重启或协议端断线期间，消息事件会丢——消息根本没进库，也就永远没人回。做法：连上 OneBot（含重连）后从协议端拉一次最近历史，把库里没有的消息按 mid 去重补进来；≤30 分钟的按新消息处理（会触发回应），更早的只补进记录、不吵人。
- **自检与静态扫描**：`src/ops.js scan`（原为 `ops/check-undefined-calls.sh` + `ops/scan-undefined-calls.py`，现已并入项目代码）。上面那次"整夜发不出一个字"的事故表现像"静默/掉线"，很难查；于是加了一个只记日志、永远 `exit 0`、不阻断启动的自检，挂在服务启动链上，另配 `src/ops.js audit` 的补丁标记检查做部署验收。

## 5. 贴纸（表情包）系统

- **自动收藏**：`src/onebot/sticker-manager.js`、`src/onebot/stickers.js`、`src/console/app.js`、`src/core/config-legacy.js`。让模型看一眼别人发的图，自己判断值不值得收（值得就存并写备注）；入口改成异步判断，不阻塞消息处理。条目保留 `srcKey` 作为去重键。
- **收藏判断健壮性**：`src/onebot/sticker-manager.js`。两个失败模式：模型有时把决定写成 `<tool_call>` 文本或裸 JSON（判断逻辑只认结构化 `tool_calls` → 决定丢失）；`max_tokens=200` 会被"思考"吃掉（实测思考 80-595 token），截断后一个字段都收不到 → 提到 600。另外内容过滤是概率性的（实测同图 20/20 通过、偶发被挡），把尝试次数 2 提到 3，并把"被服务商内容过滤"和"模型没提交"在日志里分开。
- **收藏去向与同步安全**：`src/onebot/sticker-manager.js`（优先加进 QQ 收藏表情，链接稳定、QQ 端也能用，失败退回本地库）、`src/onebot/stickers.js`（QQ 收藏列表为空或接口失败时不剪枝——否则接口一抖，本地库连同 AI 写的备注会被清空）。
- **查找与备注**：`src/onebot/stickers.js`、`src/tools/tools-core.js`、`src/onebot/sticker-manager.js`。线上连续出现 5 次"找不到表情 NNN"，编号其实来自来信里的 `[表情NNN]` 标签，模型却拿去当表情库 id 查。于是：来信把系统表情标成 `[QQ表情N 名字]`；找不到时把有效 id 回给模型；`findSticker` 增加"唯一命中"的模糊兜底，提示改为直接用备注名选图；备注上限 16 → 24 字（真图实测里 16 字会把一句话硬切）。
- **标签与收录规则**：`src/console/app.js`、`src/llm/prompt.js`、`src/tools/tools-core.js`、`src/onebot/stickers.js`、`src/onebot/sticker-manager.js`。表情包消息显示 `[表情包]`（普通图仍是 `[图片]`）；收藏规则收紧到"只认真正的表情包"，生活照/随手拍/自拍不收；相关文案统一叫"表情包"。

## 6. 主动发言与空间互动

- **开话题节奏**：`src/core/orchestrator.js`。间隔定为 2.5-3.5 小时；"没有安静的群"这种空转不算消耗本轮（45 分钟后再看）。概率、冷场阈值属于部署方偏好，脚本不强制。
- **间隔守卫**：`src/core/orchestrator.js`。tick 第一次在启动后 15 秒触发，所以每重启一次就会多一次开话题判定，与"几小时才概率开一次"的设定不符。改为把"上次判定时间"落盘，重启后不足一个间隔直接跳过（补丁标记 `minGapMs`、`writeProactiveLastAttempt`）。
- **可观测性**：`src/core/orchestrator.js`。原来整个 tick 一条日志都没有，出问题时完全没法查；现在跳过原因和真正开话题都记日志，每次 tick 最多一行，不会刷屏。
- **活跃时段**：`src/core/orchestrator.js`（支持多个窗口，如 9-12 与 14-24，窗口外不开口也不浪费间隔；调度上直接把下一次排到窗口开始）、`src/features/qzone-interactions.js`（空间互动有独立时段，不影响聊天回复）。
- **失败退避**：`src/features/qzone-interactions.js`。一次失败风暴里 3 分钟打了 191 次（失败后按 -1s 下限重排，等于每秒重试），QQ 直接回"使用人数过多，请稍后再试"。改为成功后清零失败计数、排下一次检查时加指数退避下限。回归用例见 `test/local/test-qzone-backoff.mjs`（23 秒内只尝试一次，下一次排到分钟级）。
- **抓取容错与通知阈值**：`src/features/qzone-interactions.js`、`ui/app.js`。好友动态这条外呼在腾讯侧被限流时会回 `{code:-10001, message:"network busy"}`（协议端原样透传），而它此前是硬失败：一次限流就让整轮——包括评论检查和已积压的未读——全部不跑，还会立刻顶一条"错误"级异常通知。现在抓取失败先等 45 秒重试一次（中止信号可打断等待）；仍失败只记 `run.feedError`，本轮继续跑评论检查与积压，运行记录标为「好友动态未取到」并在控制台显示原因；失败计数与退避照旧（2→4→8→16→30 分钟），连续第 3 次才发异常通知；失败轮不算建立动态基线，免得把上线前的旧动态当成新内容。用例：`test/qzone-interactions.test.mjs`、`test/local/test-qzone-backoff.mjs`、`test/local/test-qzone-intervals.mjs`。
- **每日说说容错**：`src/features/daily-moments.js`。空间列表读不到时跳过查重，不阻断发布。

## 7. 运维与控制台

- **控制台端口探测**：`src/console/integrations.js`。上游把 SnowLuma / noVNC 地址写死为旧端口 15099 / 16081，而 Linux 全栈部署实际使用 5099 / 6081，导致"服务与访问控制"页误报"不可达"。改为按实际部署端口探测，并修正改 SnowLuma 密码时的地址兜底端口。
- **控制台自动登录**：`ui/app.js`（地址栏带 `?token=` 时先自动登录，成功后清掉 URL 里的明文令牌再重载，避免留在浏览历史）、`src/console/app.js`（登录 cookie 加 `Max-Age`，避免关掉浏览器就要重新输令牌）。
- **会话列表轮询**：`ui/app.js`。首次 `startListPoller()` 在配置加载前执行会落到 4000ms 兜底值，导致会话列表每 4 秒重建一次（界面闪烁）；配置就绪后重新校准一次轮询间隔。
- **只在源码变化时重启**：部署链每小时会跑一遍所有补丁脚本，无条件重启会让服务每小时被重启多次、打断正在进行的对话；于是加哈希比对，源码没变就跳过重启。
- **运维工具与回归测试**：`src/ops.js`（单入口，详见 `docs/OPS.md`）与 `test/local/`（详见 `test/local/README.md`），均为本仓库新增。原 `ops/` 目录下的 shell/python 脚本已全部移植进 `src/ops.js`，目录本身已删除；开发机私有的 ssh 文件传输/执行脚本不再随仓库分发，远程执行直接用 `ssh`/`scp`。
- **关闭上游调试探针**：`src/*.js`、`ui/*.js`。上游作者在自己开发机上留了一批调试上报（往其私网地址的 7777/7780 端口发数据），与本项目无关，已全部关闭（守卫条件置假 + 目标地址换成本机兜底）。
