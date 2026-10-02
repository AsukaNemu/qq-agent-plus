# Rust 重构阶段 0：只读基线盘点

日期：2026-10-02（Asia/Shanghai）  
实施基线：`docs/RPD-QQ-Bot-Rust-Refactor.md` v0.3  
盘点范围：当前 checkout、源码配置、运行中的本机端口与进程；不复制密钥，不修改生产数据，不停止任何进程。

## 1. 工作区边界

盘点时 HEAD 为 `897389de56751b3442d9c0b31a5ecf3c6bbb84d9`，分支为
`main`，且工作区已有用户改动。以下路径在盘点开始前已存在修改，Rust 重构不触碰：

```text
docs/AUTO_UPDATE.md
scripts/install-service.mjs
src/console/app.js
src/core/config-legacy.js
src/features/qzone-interactions.js
src/onebot/qzone-feed.js
src/onebot/sticker-manager.js
src/tools/image-downsample.js
src/tools/tools-core.js
test/deployment-scripts.test.mjs
test/image-downsample.test.mjs
ui/app.js
```

RPD 文件本身在本次盘点前为未跟踪文件；它是本阶段唯一实施基线，不对其内容做推断式删改。

## 2. 当前运行拓扑（只读观察）

| 项目 | 观测结果 |
| --- | --- |
| 现有后端 | `/Users/hoshimorinemu/qq-agent-plus/runtime/node/bin/node src/server.js` 正在运行 |
| 现有控制台 | `127.0.0.1:3210` 正在监听 |
| QQ/NapCat 侧端口 | QQ helper 正在监听 `127.0.0.1:3000` 与 `127.0.0.1:3001` |
| 当前配置模式 | `observe`，未暂停 |
| Rust 工具链 | 已安装 stable Apple Silicon toolchain，`rustc 1.99.0`、`cargo`、`rustfmt` |
| Node 工具链 | 部署运行时 Node `v22.23.3` 可用；系统 `node`/`npm` 不在 PATH |
| checkout `data/` | 仅发现 `config.json`（27258 bytes）；没有 checkout 内 SQLite 文件 |
| 生产写入权 | 未在本阶段改变；Rust 原型不得与现有 Node 同时成为生产写入者 |

`data/config.json` 的盘点只输出字段结构、布尔开关、端口和“凭据是否存在”；没有把
`server.token`、OneBot token、模型密钥或 Cookie 写入任何产物。

## 3. 当前启用功能盘点

| 领域 | 当前值 | Rust 目标/阶段 | 最低对照样例 |
| --- | --- | --- | --- |
| 基础消息/OneBot | 生产链路存在；白名单空且 `allowAllWhenEmpty=false` | `napcat` + `policy`，阶段 1/3 | 私聊、群聊、@、去重、重连 |
| ASR | enabled | `media`/ASR adapter，阶段 5 | 语音、视频音轨、限额、失败降级 |
| 联网搜索 | enabled | `tool executor`，阶段 5 | 超时、权限、结果归档 |
| 贴纸与自动收藏 | enabled | `asset` + outbox，阶段 5 | 图片/贴纸发送与资源失效 |
| 记忆整合 | enabled | `memory`，阶段 5 | 读取、摘要、禁用保留数据 |
| 记忆 handoff | enabled | `memory`，阶段 5 | 会话接续、分页读取 |
| 身份印象 | enabled、graduated | 独立 `identity` feature，阶段 5 | 身份合并、数据迁移 |
| 异常处理 | enabled、graduated 状态独立保留 | 独立 `incident` feature，阶段 5 | 台账、通知、unknown write |
| 主动消息 | disabled | 延后；保留 Node 数据与配置 | 开关关闭无定时器/外写 |
| Daily Moments | disabled | 延后；保留数据 | 定时任务重复防护 |
| QZone interactions | disabled | 延后；保留数据 | 读取与外部写入状态 |
| 自动更新 | disabled | 运维阶段单独迁移 | 备份、升级、回滚 |
| Slang pilot | disabled | 延后/按 RPD 矩阵决策 | 数据保留，不因关闭删除 |

## 4. 现有代码边界

- OneBot 原始 JSON 当前集中在 `src/onebot/onebot.js` 与
  `src/console/app.js` 的事件入口；发送走 OneBot HTTP，接收走 WebSocket。
- 当前入口按会话串行化事件，再写 `messages.sqlite`/outbox 并进入 orchestrator。
- 当前 Node store 已使用 SQLite WAL、`synchronous=FULL`、消息去重、租约和 outbox；
  Rust migration 必须保留这些可观察语义，不能仅按新表名重置状态。
- 现有 UI 与控制台 API 仍是生产入口；本阶段只新增独立 `rinbot/` 原型，未改旧 UI。
- 现有配置包含 OneBot 地址、allow/deny、运行模式、功能开关、模型和存储参数；Rust
  配置适配必须采用掩码响应，不能把密钥复制到前端或日志。

## 5. 验证记录

已通过：

```text
/Users/hoshimorinemu/qq-agent-plus/runtime/node/bin/node --version
v22.23.3

runtime Node test/layout.test.mjs
4 passed, 0 failed

runtime Node test/*.test.mjs
699 tests total: 669 passed, 21 failed, 9 skipped
```

21 个失败全部来自已有 `test/deploy-all-preflight.test.mjs` 场景，统一在复制出的
`deploy-all.sh:475` 因 `IMAGE_MIRRORS[@]: unbound variable` 退出；没有失败落在本次
新增 `rinbot/` 或 `docs/rust-refactor/` 文件。该问题留给部署脚本专项处理，本次不跨范围修改。

补充验证：

- Rust `cargo test --manifest-path rinbot/Cargo.toml`：10 个测试通过，0 失败；包含 7 个
  单元测试和 3 个 OneBot fixture 集成测试。
- `cargo fmt -- --check`：通过。
- API smoke test：临时数据目录、`127.0.0.1:3211` 启动成功；health/status/snapshot/messages/
  unknown outbox/SSE hello 均通过，随后已停止临时进程。
- 测试数据隔离：完整 Node 测试已使用部署自带 Node 运行；测试数据仍只存在临时目录，
  checkout `data/` 仍只有原有 `config.json`。失败原因见上，不将其归因于 Rust 工程。
- 生产 NapCat 对照：按 RPD 阶段 1 的观察/回放门禁执行，尚未给 Rust 原型生产发送权限。
- RInBot 前端：固定 RhineLabUI commit `ee5779741c6c0c916e416705fa634c7abf905c73`；
  `rinbot/ui` 直接使用其页面、Three.js 场景、模型、音频和档案内容，并通过状态桥接显示
  Rust 核心状态。`tsc --noEmit` 与 `vite build` 通过；档案正文尚待逐项替换为 RInBot
  真实业务数据。

## 6. 阶段 0 结论

1. 可以安全开始 Rust 基础工程，但必须使用独立数据目录与非 `3210` 端口。
2. Rust 默认 `observe`，默认不连接 NapCat；显式连接也只持久化独立观察数据，禁止发送。
3. Node、NapCat、Python 代理和旧前端均继续保持原样，直到迁移矩阵和对账门槛完成。
4. 当前工作区用户改动被视为外部变更，Rust 目录是本轮新增边界。
