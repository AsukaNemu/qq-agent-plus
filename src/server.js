// Linux 服务入口：node src/server.js
import { createApp } from './console/app.js';
import { installManualFriendReviewRoute } from './console/manual-friend-review-route.js';
import { installExperimentalMultimodalContextPilot } from './pilots/experimental-multimodal-context.js';
import { probeContainerReadable } from './tools/tools-core.js';

let app = null;
process.on('unhandledRejection', (error) => {
  app?.captureIncident(error, {
    source: 'process',
    category: 'process',
    severity: 'critical',
    code: 'UNHANDLED_REJECTION'
  });
  console.error('[未处理异常]', error);
});
process.on('uncaughtException', (error) => {
  app?.captureIncident(error, {
    source: 'process',
    category: 'process',
    severity: 'critical',
    code: 'UNCAUGHT_EXCEPTION'
  });
  console.error('[未捕获异常]', error);
  process.exit(1);
});

// 仅安装一次薄包装；开关关闭时 multimodal-context commit 原样委托旧实现。
installExperimentalMultimodalContextPilot();

// 启动自检：服务进程能不能读写 QQ 容器（决定 get_image 兜底是否可用）。
// 只在日志里说一次，不参与业务逻辑 —— 见 tools-core.js 里 probeContainerReadable 的说明。
try { probeContainerReadable(); } catch { /* 自检失败不影响启动 */ }

app = createApp();
installManualFriendReviewRoute(app);
app.start().catch((error) => {
  console.error('[启动失败]', error);
  process.exit(1);
});

let stopping = false;
async function shutdown() {
  if (stopping) return;
  stopping = true;
  const deadline = setTimeout(() => process.exit(1), 25000);
  deadline.unref();
  try { await app.stop(); process.exit(0); }
  catch (error) { console.error(error); process.exit(1); }
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
