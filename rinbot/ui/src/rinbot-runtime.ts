import { apiFetch } from "./api-client.ts";

export type RuntimeSnapshot = {
  version: number;
  status: {
    mode: string;
    onebot_connected: boolean;
    core_ready: boolean;
    queue_depth: number;
    queue_capacity: number;
    uptime_seconds: number;
    migration_version: number;
  };
  counts: { messages: number; queued_inbox: number; unknown_outbox: number };
};

export type RuntimeMessage = {
  conversation_id: string;
  sender_id: string;
  is_group: boolean;
  text: string;
  timestamp: number;
};

export type RuntimeUnknown = {
  id: string;
  conversation_id: string;
  payload: string;
  attempts: number;
  updated_at: number;
  error?: string;
};

export type RuntimeTask = {
  id: number;
  conversation_id: string;
  state: "queued" | "processing" | "held" | "failed" | string;
  attempts: number;
  received_at: number;
};

export type ConversationHandoff = {
  topic: string;
  summary: string;
  hypotheses: string[];
  evidence: string[];
  facts: string[];
  decisions: string[];
  rejectedDirections: string[];
  openQuestions: string[];
  nextStep: string;
  participantIds: string[];
  lastReply: string;
  updatedAt: number;
  expiresAt: number;
};

export type ConversationHandoffRecord = {
  chatKey: string;
  handoff: ConversationHandoff;
};

export type StickerArchiveEntry = {
  id: string;
  description: string;
  note: string;
  tags: string[];
  usage: string;
  source: string;
  hidden: boolean;
  sendable: boolean;
  sendErrorCode: string;
  sendErrorMessage: string;
  sendErrorAt: number;
  useCount: number;
  lastUsedAt: number;
  hasImage: boolean;
};

export type LegacyIdentityRecord = {
  [key: string]: unknown;
  uin?: string;
  userId?: string;
  primary_name?: string;
  is_friend?: number;
  first_seen_at?: number;
  last_seen_at?: number;
  message_count?: number;
  id?: string;
  status?: string;
  comment?: string;
  created_at?: number;
  decision?: string;
  actionError?: string;
};

export type RuntimeConfig = {
  host: string;
  port: number;
  data_dir: string;
  ui_dir: string;
  onebot_ws_url: string;
  onebot_http_url: string;
  connect_onebot: boolean;
  observe_only: boolean;
  time_control_enabled: boolean;
  identity_pilot_enabled: boolean;
  identity_pilot_graduated: boolean;
  incoming_friend_request_enabled: boolean;
  incoming_friend_auto_whitelist: boolean;
  identity_owner_configured: boolean;
  identity_decisions_available: boolean;
  identity_management_available: boolean;
  api_token_configured: boolean;
  onebot_token_configured: boolean;
};

export type QueueSettings = {
  wake_delay_min_ms: number;
  wake_delay_max_ms: number;
  max_batch_wait_ms: number;
  batch_limit: number;
  batch_max_chars: number;
  max_concurrent_runs: number;
};

export type WebSearchSettings = {
  enabled: boolean;
  provider: string;
  search_url: string;
  max_results: number;
};

export type AsrSettings = {
  enabled: boolean;
  provider: string;
  base_url: string;
  model: string;
  language: string;
  max_per_hour: number;
  timeout_ms: number;
};

export type AsrSettingsResponse = {
  settings: AsrSettings;
  configured: boolean;
  apiKeyConfigured: boolean;
};

export type TimeControlSettings = {
  enabled: boolean;
  schedule: { mode: string; windows: Array<{ days: number[]; start: string; end: string }> };
  overrides: Record<string, { mode: string; windows: Array<{ days: number[]; start: string; end: string }> }>;
};

export type BehaviorSettings = {
  access: {
    allow_groups: string[];
    allow_private: string[];
    deny_groups: string[];
    deny_private: string[];
    allow_all_when_empty: boolean;
    blocked_group_users: Record<string, string[]>;
  };
  triggers: {
    random_percent: number;
    keywords: string[];
    at_count: number;
    keyword_count: number;
    random_count: number;
    all_count: number;
    unified_tier: boolean;
    group_random_percent: Record<string, number>;
  };
};

export type ModelSettings = {
  base_url: string;
  model: string;
  system_prompt: string;
  temperature: number;
  max_tokens: number;
  max_tool_rounds: number;
  timeout_ms: number;
  fallback: {
    enabled: boolean;
    base_url: string;
    model: string;
    timeout_ms: number;
  };
  vision_model: {
    enabled: boolean;
    base_url: string;
    model: string;
    timeout_ms: number;
  };
};

export type ModelSettingsResponse = {
  settings: ModelSettings;
  api_key_configured: boolean;
  fallback_api_key_configured: boolean;
  vision_api_key_configured: boolean;
};

export type RuntimeIncident = {
  id: string;
  code: string;
  rawCode: string;
  errorCode: string;
  errorNote: string;
  category: string;
  severity: string;
  source: string;
  chatKey: string;
  state: string;
  count: number;
  firstAt: number;
  lastAt: number;
  message: string;
  details: Record<string, unknown>;
  notifyState: string;
  notifyError: string;
  resolution: string;
};

export type IncidentStatus = {
  enabled: boolean;
  graduated: boolean;
  active: boolean;
  exists: boolean;
  error: string;
  open: number;
  acknowledged: number;
  resolved: number;
  critical: number;
  pendingNotifications: number;
};

export type IncidentSettings = {
  enabled: boolean;
  graduated: boolean;
  notifyWarnings: boolean;
  duplicateWindowMinutes: number;
  retentionDays: number;
  unknownWritesBlockChat: boolean;
};

export type RelationshipPilotStatus = {
  enabled: boolean;
  graduated: boolean;
  active: boolean;
  exists: boolean;
  states: number;
  events: number;
  flags: number;
  cursors: number;
  evaluations: number;
  error: string;
};

export type RelationshipPilotSettings = {
  enabled: boolean;
  graduated: boolean;
  model: string;
  minNewMessages: number;
  minBatchMessages: number;
  minBatchAgeMinutes: number;
  maxEvidenceMessages: number;
  maxPeoplePerTurn: number;
  maxEvaluationsPerDay: number;
  familiarityRefreshMinutes: number;
  frictionHalfLifeHours: number;
  maxMemoryContext: number;
};

export type QzoneInteractionsStatus = {
  enabled: boolean;
  graduated: boolean;
  active: boolean;
  observing: boolean;
  runtimeReady: boolean;
  exists: boolean;
  items: number;
  actionStates: Record<string, number>;
  error: string;
};

export type ProactiveSettings = {
  enabled: boolean;
  graduated: boolean;
  checkIntervalMinMs: number;
  checkIntervalMaxMs: number;
  idleThresholdMs: number;
  probability: number;
  followUpEnabled: boolean;
  selfWakeEnabled: boolean;
  activeHours: { start: string; end: string }[] | null;
};

export type ProactiveStatus = {
  enabled: boolean;
  graduated: boolean;
  active: boolean;
  runtimeReady: boolean;
  exists: boolean;
  states: Record<string, number>;
};

export type StickerLifecycle = {
  settings: { enabled: boolean; graduated: boolean; promptMaxStickers: number; encourage: number };
  active: boolean;
  runtimeReady: boolean;
};

export type StickerCollectionLifecycle = {
  settings: { enabled: boolean; graduated: boolean };
  active: boolean;
  runtimeReady: boolean;
};

export type MemoryConsolidationStatus = {
  enabled: boolean;
  graduated: boolean;
  active: boolean;
  runtimeReady: boolean;
  manualReady: boolean;
  executorAvailable: boolean;
  automaticSchedulingReady: boolean;
  cooldownStoreAvailable: boolean;
  revisionStoreAvailable: boolean;
  manualRun: MemoryConsolidationRunResult & {
    state: "idle" | "running" | "completed" | "cancelled" | "failed";
    currentIndex?: number;
    currentUserId?: string;
    error?: string;
  };
  automaticSettings: {
    minImpressions: number;
    maxPerMember: number;
    minimumMemberMessages: number;
    discoverMinMessages: number;
    discoverMaxMembers: number;
    minIntervalMs: number;
  };
};

export type MultimodalContextStatus = {
  enabled: boolean;
  graduated: boolean;
  active: boolean;
  runtimeReady: boolean;
  handoffAvailable: boolean;
  messageImageReadAvailable: boolean;
  stickerImageReadAvailable: boolean;
};

export type MemoryConsolidationPersonRunResult = {
  userId: string;
  revision: number;
  previousCount: number;
  impressionCount: number;
  sourceMessageCount: number;
  changed: boolean;
};

export type MemoryConsolidationRunResult = {
  state?: "idle" | "running" | "completed" | "cancelled" | "failed";
  error?: string;
  currentIndex?: number;
  currentUserId?: string;
  chatKey: string;
  userId?: string;
  revision?: number;
  previousCount?: number;
  impressionCount?: number;
  sourceMessageCount?: number;
  changed?: boolean;
  requested: number;
  completed: number;
  results: MemoryConsolidationPersonRunResult[];
  skipped: Array<{ userId: string; reason: string }>;
  failed: Array<{ userId: string; reason: string }>;
};

export type MemoryConsolidationRevision = {
  revision: number;
  createdAt: number;
  impressions: string[];
  sourceMessageCount: number;
  legacySnapshot: boolean;
};

export type NodeMemoryConsolidationBackup = {
  backupKey: string;
  backedUpAt: number;
  reason: string;
  sourceChatKey: string;
  impressions: Array<{
    content: string;
    createdAt: number;
    observedAt: number;
    origin: string;
    sourceChatKeys: string[];
  }>;
};

export type MemoryConsolidationHistory = {
  chatKey: string;
  userId: string;
  currentRevision: number;
  revisions: MemoryConsolidationRevision[];
  nodeBackups: NodeMemoryConsolidationBackup[];
};

export type ModelMemoryMutationEntry = {
  kind?: string;
  id?: number;
  content?: string;
  chatKey?: string;
};

export type ModelMemoryMutation = {
  operationId: string;
  userId: string;
  chatKey: string;
  action: string;
  before: { affected?: { userId: string; entries: ModelMemoryMutationEntry[] }[]; removalOperationId?: string } | { userId: string; entries: ModelMemoryMutationEntry[] }[];
  after: unknown;
  createdAt: number;
  restored: boolean;
};

export type ModelMemoryMutationHistory = {
  chatKey: string;
  userId: string;
  mutations: ModelMemoryMutation[];
};

export type ProactiveWake = {
  id: string;
  conversationId: string;
  kind: string;
  at: number;
  note: string;
  paced: boolean;
  state: string;
  attemptId: string;
  outboxId: string;
  error: string;
  createdAt: number;
  updatedAt: number;
};

export type QzoneInteractionsAudit = {
  runs: Array<{ id?: string; status?: string; startedAt?: number; updatedAt?: number; decisionCount?: number; actionCount?: number; doneCount?: number; unknownCount?: number }>;
  actions: Array<{ actionId: string; runId: string; itemKind: string; itemKey: string; action: string; state: string; attemptCount: number; error: string; createdAt: number; updatedAt: number }>;
};

export type QzoneInteractionsSettings = {
  enabled: boolean;
  graduated: boolean;
  startupCatchup: boolean;
  activeHours: { start: string; end: string } | null;
  feedIntervalMinutes: number;
  replyIntervalMinutes: number;
  feedFetchCount: number;
  ownPostCount: number;
  maxAgeHours: number;
  maxBatchItems: number;
  decisionInputBudgetTokens: number;
  thinkingRequestPatch: Record<string, unknown> | null;
  maxLikesPerRun: number;
  maxCommentsPerRun: number;
  maxRepliesPerRun: number;
  commentMaxChars: number;
  replyMaxChars: number;
  allowLikes: boolean;
  allowComments: boolean;
  allowReplies: boolean;
  actionDelayMinMs: number;
  actionDelayMaxMs: number;
  maxDecisionRounds: number;
};

export type RelationshipState = {
  uin: string;
  familiarity: number;
  affinity: number;
  friction: number;
  messageCount: number;
  activeDays: number;
  directInteractions: number;
  chatCount: number;
  lastInteractionAt: number;
  lastEvaluatedAt: number;
};

export type RelationshipDetail = {
  person: RelationshipState & { userId?: string };
  events: Array<{ id: string; eventType: string; polarity: number; summary: string; confidence: number; appliedAt: number }>;
  flags: Array<{ id: string; flagType: string; status: string; createdAt: number }>;
  cursors: Record<string, number>;
  evaluations: Array<{
    id: string; status: string; evidenceCount: number; sourceChatKeys: string[]; model: string;
    usage: { promptTokens?: number; completionTokens?: number; totalTokens?: number; cachedTokens?: number };
    hasError: boolean; createdAt: number; completedAt: number;
  }>;
  evaluationCount: number;
  evaluationLimit: number;
  evaluationOffset: number;
};

export type RelationshipEvaluationTrace = {
  id: string;
  status: string;
  model: string;
  request: unknown;
  response: unknown;
  session: unknown;
  createdAt: number;
  completedAt: number;
};

export type ChatControl = {
  chatKey: string;
  mode: "auto" | "blocked" | "continue" | string;
  reason: string;
  updatedAt: number;
  updatedBy: string;
  version: number;
};

export type ChatControlView = {
  control: ChatControl;
  decision: { allowed: boolean; effectiveState: string; reason: string; held: number };
  held: number;
};

export type GroupInviteRecord = {
  id: string; groupId: string; inviterId: string; subtype: string; comment: string; reviewReason: string;
  state: string; decision: string; actionError: string; createdAt: number;
  whitelistState: string; whitelistError: string; notifyState: string;
  notifyError: string; notifyMessageId: string; notifiedAt: number;
};

export type GroupInviteSettings = {
  enabled: boolean;
  friendsOnly: boolean;
  autoWhitelist: boolean;
  notifyOwner: boolean;
};

export type GroupInviteSettingsResponse = {
  settings: GroupInviteSettings;
  runtimeReady: boolean;
  ownerConfigured: boolean;
  readOnly: boolean;
};

export type GroupInviteDirectEntry = {
  id: string; groupId: string; userId: string; operatorId: string;
  joinedAt: number; createdAt: number; whitelistState: string;
  whitelistError: string; notifyState: string; notifyError: string;
  notifyMessageId: string; notifiedAt: number;
};

export type DailyMomentRecord = {
  [key: string]: unknown;
  id?: string;
  dayKey?: string;
  source?: string;
  status?: string;
  decision?: string;
  reason?: string;
  content?: string;
  error?: string;
  groupCount?: number;
  publishAttempted?: boolean;
  tid?: string;
};

export type DailyMomentsSettings = {
  enabled: boolean;
  hour: number;
  minute: number;
  scheduleWindows: { start: string; end: string; count: number }[] | null;
  intervalDays: number;
  startupCatchup: boolean;
  minMessagesPerGroup: number;
  maxGroups: number;
  maxMessagesPerGroup: number;
  maxPromptChars: number;
  allowImages: boolean;
  maxImages: number;
  visibility: number;
  targetUins: string[];
  maxResearchCalls: number;
  maxRounds: number;
};

export type DailyMomentScheduleSlot = {
  [key: string]: unknown;
  id?: string;
  dayKey?: string;
  status?: string;
  reason?: string;
  at?: number;
};

export type DailyMomentGenerationDecision = {
  slotId: string;
  dayKey: string;
  outcome: "draft" | "skipped";
  createdAt: number;
  decision: {
    decision?: "publish" | "skip";
    reason?: string;
    content?: string;
    groupSummaries?: { chatKey: string; groupName: string; summary: string }[];
    researchSources?: string[];
  };
};

export const runtime = {
  health: null as { version: string; name: string } | null,
  snapshot: null as RuntimeSnapshot | null,
  config: null as RuntimeConfig | null,
  queueSettings: null as QueueSettings | null,
  webSearchSettings: null as WebSearchSettings | null,
  asrSettings: null as AsrSettingsResponse | null,
  timeControlSettings: null as TimeControlSettings | null,
  behaviorSettings: null as BehaviorSettings | null,
  modelSettings: null as ModelSettingsResponse | null,
  incidentStatus: null as IncidentStatus | null,
  incidentSettings: null as IncidentSettings | null,
  relationshipStatus: null as RelationshipPilotStatus | null,
  relationshipSettings: null as RelationshipPilotSettings | null,
  qzoneInteractionsStatus: null as QzoneInteractionsStatus | null,
  qzoneInteractionsSettings: null as QzoneInteractionsSettings | null,
  qzoneInteractionsAudit: null as QzoneInteractionsAudit | null,
  qzoneInteractionsAuditError: "",
  qzoneInteractionsAuditLoaded: false,
  proactiveStatus: null as ProactiveStatus | null,
  stickerLifecycle: null as StickerLifecycle | null,
  stickerCollectionLifecycle: null as StickerCollectionLifecycle | null,
  memoryConsolidationStatus: null as MemoryConsolidationStatus | null,
  multimodalContextStatus: null as MultimodalContextStatus | null,
  memoryConsolidationRunResult: null as MemoryConsolidationRunResult | null,
  memoryConsolidationRunError: "",
  memoryConsolidationHistory: null as MemoryConsolidationHistory | null,
  memoryConsolidationHistoryError: "",
  modelMemoryMutationHistory: null as ModelMemoryMutationHistory | null,
  modelMemoryMutationHistoryError: "",
  proactiveSettings: null as ProactiveSettings | null,
  proactiveWakes: [] as ProactiveWake[],
  proactiveLoaded: false,
  proactiveError: "",
  relationships: [] as RelationshipState[],
  relationshipDetail: null as RelationshipDetail | null,
  relationshipDetailError: "",
  relationshipEvaluationTrace: null as RelationshipEvaluationTrace | null,
  relationshipEvaluationTraceError: "",
  incidents: [] as RuntimeIncident[],
  memoryHandoffs: [] as ConversationHandoffRecord[],
  memoryHandoffError: "",
  memoryHandoffsLoaded: false,
  stickers: [] as StickerArchiveEntry[],
  stickerPreviewUrls: {} as Record<string, string>,
  stickerQuery: "",
  stickerArchiveError: "",
  stickerArchiveLoaded: false,
  stickerSyncStatus: "",
  stickerSyncInProgress: false,
  chatControls: [] as ChatControlView[],
  groupInvites: [] as GroupInviteRecord[],
  groupInviteDirectEntries: [] as GroupInviteDirectEntry[],
  groupInviteSettings: null as GroupInviteSettingsResponse | null,
  dailyMomentRecords: [] as DailyMomentRecord[],
  dailyMomentScheduleSlots: [] as DailyMomentScheduleSlot[],
  dailyMomentGenerationDecisions: [] as DailyMomentGenerationDecision[],
  dailyMomentSettings: null as DailyMomentsSettings | null,
  dailyMomentGenerationReady: false,
  dailyMomentGenerationAvailable: false,
  dailyMomentRuntimeReady: false,
  dailyMomentSettingsError: "",
  dailyMomentArchiveError: "",
  dailyMomentArchiveLoaded: false,
  dailyMomentRunInProgress: false,
  dailyMomentRunStatus: "",
  messages: [] as RuntimeMessage[],
  tasks: [] as RuntimeTask[],
  identityPeople: [] as LegacyIdentityRecord[],
  identityPeopleSource: "",
  identityFriendRequests: [] as LegacyIdentityRecord[],
  identityRequestSource: "",
  unknown: [] as RuntimeUnknown[],
  connected: false,
  apiErrors: {} as Record<string, string>,
  events: [] as Array<{ at: number; label: string }>,
  messageOffset: 0,
  taskOffset: 0,
  pageSize: 50,
  pageLoading: false,
};

export async function loadArchivePage(kind: "messages" | "tasks", offset: number): Promise<void> {
  if (runtime.pageLoading) return;
  runtime.pageLoading = true;
  listeners.forEach((listener) => listener());
  const path = `/api/${kind}`;
  try {
    const response = await apiFetch(`${path}?limit=${runtime.pageSize}&offset=${Math.max(0, offset)}`, { credentials: "same-origin" });
    if (!response.ok) throw new Error(`读取失败（HTTP ${response.status}）`);
    const data = await response.json();
    if (kind === "messages") { runtime.messages = data.messages; runtime.messageOffset = Math.max(0, offset); }
    else { runtime.tasks = data.items; runtime.taskOffset = Math.max(0, offset); }
    delete runtime.apiErrors[path];
  } catch (error) {
    runtime.apiErrors[path] = error instanceof Error ? error.message : "读取失败，请重试";
  } finally {
    runtime.pageLoading = false;
    listeners.forEach((listener) => listener());
  }
}

export async function loadMemoryHandoffs(adminToken: string): Promise<void> {
  runtime.memoryHandoffError = "";
  const response = await apiFetch("/api/memory/handoffs?limit=100&offset=0", {
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  const result = await response.json().catch(() => ({})) as {
    items?: ConversationHandoffRecord[];
    error?: string;
  };
  if (!response.ok) {
    runtime.memoryHandoffError = result.error || `读取失败（HTTP ${response.status}）`;
    runtime.memoryHandoffsLoaded = false;
  } else {
    runtime.memoryHandoffs = result.items ?? [];
    runtime.memoryHandoffsLoaded = true;
  }
  listeners.forEach((listener) => listener());
}

export async function loadStickerArchive(adminToken: string, query = ""): Promise<void> {
  runtime.stickerArchiveError = "";
  runtime.stickerQuery = query;
  Object.values(runtime.stickerPreviewUrls).forEach((url) => URL.revokeObjectURL(url));
  runtime.stickerPreviewUrls = {};
  const params = new URLSearchParams({ q: query, limit: "100", offset: "0" });
  const response = await apiFetch(`/api/stickers?${params}`, {
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  const result = await response.json().catch(() => ({})) as {
    items?: StickerArchiveEntry[];
    error?: string;
    runtimeReady?: boolean;
  };
  if (!response.ok) {
    runtime.stickerArchiveError = result.error || `读取失败（HTTP ${response.status}）`;
    runtime.stickerArchiveLoaded = false;
  } else {
    runtime.stickers = result.items ?? [];
    runtime.stickerArchiveLoaded = true;
  }
  listeners.forEach((listener) => listener());
}

export async function loadStickerPreview(adminToken: string, stickerId: string): Promise<string | null> {
  const response = await apiFetch(`/api/stickers/${encodeURIComponent(stickerId)}/image`, {
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    throw new Error(result.error || `图片读取失败（HTTP ${response.status}）`);
  }
  const previous = runtime.stickerPreviewUrls[stickerId];
  if (previous) URL.revokeObjectURL(previous);
  const url = URL.createObjectURL(await response.blob());
  runtime.stickerPreviewUrls[stickerId] = url;
  listeners.forEach((listener) => listener());
  return url;
}

export async function createStickerEntry(
  adminToken: string,
  imageDataUrl: string,
  fields: { desc: string; localNote: string; tags: string[]; usage: string },
  imageUrl = "",
): Promise<string | null> {
  const response = await apiFetch("/api/stickers", {
    method: "POST", credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}`, "content-type": "application/json" },
    body: JSON.stringify({ imageDataUrl, imageUrl, ...fields }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `新增失败（HTTP ${response.status}）`;
  await loadStickerArchive(adminToken, runtime.stickerQuery);
  return null;
}

export async function updateStickerEntry(
  adminToken: string,
  stickerId: string,
  fields: { desc: string; localNote: string; tags: string[]; usage: string },
): Promise<string | null> {
  const response = await apiFetch(`/api/stickers/${encodeURIComponent(stickerId)}`, {
    method: "PUT", credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}`, "content-type": "application/json" },
    body: JSON.stringify(fields),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `保存失败（HTTP ${response.status}）`;
  await loadStickerArchive(adminToken, runtime.stickerQuery);
  return null;
}

export async function deleteStickerEntry(adminToken: string, stickerId: string): Promise<string | null> {
  const response = await apiFetch(`/api/stickers/${encodeURIComponent(stickerId)}`, {
    method: "DELETE", credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}`, "content-type": "application/json" },
    body: JSON.stringify({ confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string; cleanupPending?: boolean; hidden?: boolean };
  if (!response.ok) return result.error || `删除失败（HTTP ${response.status}）`;
  await loadStickerArchive(adminToken, runtime.stickerQuery);
  return result.cleanupPending ? "档案已移除；图片文件清理待检查" : result.hidden ? "QQ 收藏已在本地隐藏，未操作 QQ 收藏" : null;
}

export async function syncStickerFavorites(adminToken: string): Promise<void> {
  runtime.stickerSyncStatus = "正在从 NapCat 读取最多 500 条 QQ 收藏…";
  runtime.stickerSyncInProgress = true;
  listeners.forEach((listener) => listener());
  try {
    const response = await apiFetch("/api/stickers/sync", {
      method: "POST",
      credentials: "same-origin",
      headers: {
        authorization: `Bearer ${adminToken}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({ confirm: true }),
    });
    const result = await response.json().catch(() => ({})) as {
      summary?: { added?: number; updated?: number; removed?: number; preservedLocal?: number };
      error?: string;
    };
    if (!response.ok) {
      runtime.stickerSyncStatus = result.error || `同步失败（HTTP ${response.status}）；本地档案未变更`;
      return;
    }
    const summary = result.summary ?? {};
    runtime.stickerSyncStatus = `同步完成：新增 ${summary.added ?? 0}、更新 ${summary.updated ?? 0}、移除 ${summary.removed ?? 0}、保留本地条目 ${summary.preservedLocal ?? 0}`;
    await loadStickerArchive(adminToken, runtime.stickerQuery);
  } catch {
    runtime.stickerSyncStatus = "同步请求失败；请确认连接状态后重试";
  } finally {
    runtime.stickerSyncInProgress = false;
    listeners.forEach((listener) => listener());
  }
}

export async function loadQzoneInteractionsAudit(adminToken: string): Promise<void> {
  runtime.qzoneInteractionsAuditError = "";
  const response = await apiFetch("/api/qzone-interactions/audit?limit=50", {
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  const result = await response.json().catch(() => ({})) as QzoneInteractionsAudit & { error?: string };
  if (!response.ok) {
    runtime.qzoneInteractionsAuditError = result.error || `读取失败（HTTP ${response.status}）`;
    runtime.qzoneInteractionsAuditLoaded = false;
  } else {
    runtime.qzoneInteractionsAudit = result;
    runtime.qzoneInteractionsAuditLoaded = true;
  }
  listeners.forEach((listener) => listener());
}

export async function runQzoneInteractions(kind: "all" | "feed" | "reply", adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/qzone-interactions/run", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ kind, confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as { queued?: boolean; error?: string };
  if (!response.ok) return result.error || `手动运行请求失败（HTTP ${response.status}）`;
  await refreshRuntime();
  return result.queued ? "手动运行已排入空间互动调度器；请在审核记录中查看结果。" : "手动运行请求已处理。";
}

export async function loadDailyMomentArchive(adminToken: string): Promise<void> {
  runtime.dailyMomentArchiveError = "";
  const headers = { authorization: `Bearer ${adminToken}` };
  const [response, settingsResponse] = await Promise.all([
    apiFetch("/api/daily-moments/archive?limit=100&offset=0", {
      credentials: "same-origin", headers,
    }),
    apiFetch("/api/settings/daily-moments", { credentials: "same-origin", headers }),
  ]);
  const result = await response.json().catch(() => ({})) as {
    items?: DailyMomentRecord[];
    scheduleSlots?: DailyMomentScheduleSlot[];
    generationDecisions?: DailyMomentGenerationDecision[];
    error?: string;
  };
  const settingsResult = await settingsResponse.json().catch(() => ({})) as {
    settings?: DailyMomentsSettings;
    generationReady?: boolean;
    generationAvailable?: boolean;
    runtimeReady?: boolean;
    error?: string;
  };
  if (!response.ok) {
    runtime.dailyMomentArchiveError = result.error || `读取失败（HTTP ${response.status}）`;
    runtime.dailyMomentArchiveLoaded = false;
  } else {
    runtime.dailyMomentRecords = result.items ?? [];
    runtime.dailyMomentScheduleSlots = result.scheduleSlots ?? [];
    runtime.dailyMomentGenerationDecisions = result.generationDecisions ?? [];
    runtime.dailyMomentArchiveLoaded = true;
  }
  if (!settingsResponse.ok) {
    runtime.dailyMomentSettings = null;
    runtime.dailyMomentSettingsError = settingsResult.error || `读取配置失败（HTTP ${settingsResponse.status}）`;
  } else {
    runtime.dailyMomentSettings = settingsResult.settings ?? null;
    runtime.dailyMomentGenerationReady = settingsResult.generationReady ?? false;
    runtime.dailyMomentGenerationAvailable = settingsResult.generationAvailable ?? false;
    runtime.dailyMomentRuntimeReady = settingsResult.runtimeReady ?? false;
    runtime.dailyMomentSettingsError = "";
  }
  listeners.forEach((listener) => listener());
}

export async function runDailyMomentPreview(adminToken: string): Promise<string | null> {
  runtime.dailyMomentRunInProgress = true;
  runtime.dailyMomentRunStatus = "正在读取当日素材并生成预览；不会发布到 QQ 空间。";
  listeners.forEach((listener) => listener());
  try {
    const response = await apiFetch("/api/daily-moments/run", {
      method: "POST",
      credentials: "same-origin",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${adminToken}`,
      },
      body: JSON.stringify({ publish: false }),
    });
    const result = await response.json().catch(() => ({})) as { error?: string; status?: string };
    if (!response.ok) {
      runtime.dailyMomentRunStatus = result.error || `生成失败（HTTP ${response.status}）`;
      return runtime.dailyMomentRunStatus;
    }
    runtime.dailyMomentRunStatus = result.status === "skipped"
      ? "本次没有达到生成条件；跳过原因已记入档案。"
      : "预览草稿已生成并保存；尚未发布。";
    await loadDailyMomentArchive(adminToken);
    return null;
  } catch {
    runtime.dailyMomentRunStatus = "请求未完成。请先检查归档里的任务状态，不要直接重复触发。";
    return runtime.dailyMomentRunStatus;
  } finally {
    runtime.dailyMomentRunInProgress = false;
    listeners.forEach((listener) => listener());
  }
}

export async function saveDailyMomentsSettings(
  settings: DailyMomentsSettings,
  adminToken: string,
): Promise<string | null> {
  const response = await apiFetch("/api/settings/daily-moments/update", {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ settings }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `保存失败（HTTP ${response.status}）`;
  await loadDailyMomentArchive(adminToken);
  return null;
}

export async function resolveDailyMomentPublication(
  id: string,
  outcome: "sent" | "missed",
  adminToken: string,
  note: string,
  tid?: string,
): Promise<string | null> {
  const response = await apiFetch(`/api/daily-moments/records/${encodeURIComponent(id)}/resolve`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ confirm: true, outcome, note, ...(tid ? { tid } : {}) }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `核对失败（HTTP ${response.status}）`;
  await loadDailyMomentArchive(adminToken);
  return null;
}

export async function publishDailyMomentDraft(
  id: string,
  adminToken: string,
): Promise<string | null> {
  const response = await apiFetch(`/api/daily-moments/records/${encodeURIComponent(id)}/publish`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `发布失败（HTTP ${response.status}）`;
  await loadDailyMomentArchive(adminToken);
  return null;
}

export async function clearMemoryHandoff(chatKey: string, adminToken: string): Promise<string | null> {
  const response = await apiFetch(`/api/memory/handoffs/${encodeURIComponent(chatKey)}`, {
    method: "DELETE",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `清除失败（HTTP ${response.status}）`;
  runtime.memoryHandoffs = runtime.memoryHandoffs.filter((item) => item.chatKey !== chatKey);
  listeners.forEach((listener) => listener());
  return null;
}

export async function saveMemoryHandoff(
  chatKey: string,
  expectedUpdatedAt: number,
  handoff: Pick<ConversationHandoff, "topic" | "summary" | "hypotheses" | "evidence" | "facts" | "decisions" | "rejectedDirections" | "openQuestions" | "nextStep"> & { ttlMinutes: number },
  adminToken: string,
): Promise<string | null> {
  const response = await apiFetch(`/api/memory/handoffs/${encodeURIComponent(chatKey)}`, {
    method: "PUT",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ ...handoff, expectedUpdatedAt }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `保存失败（HTTP ${response.status}）`;
  await loadMemoryHandoffs(adminToken);
  return runtime.memoryHandoffError || null;
}

const listeners = new Set<() => void>();

export function subscribeRuntime(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

let refreshInFlight: Promise<void> | null = null;
export async function refreshRuntime(): Promise<void> {
  if (refreshInFlight) return refreshInFlight;
  refreshInFlight = refreshRuntimeData().finally(() => { refreshInFlight = null; });
  return refreshInFlight;
}
async function refreshRuntimeData(): Promise<void> {
  const routes: Array<[string, (data: any) => void]> = [
    ["/api/health", (data) => { runtime.health = data; }],
    ["/api/snapshot", (data) => { runtime.snapshot = data; }],
    ["/api/config", (data) => { runtime.config = data; }],
    ["/api/settings/queue", (data) => { runtime.queueSettings = data; }],
    ["/api/settings/time-control", (data) => { runtime.timeControlSettings = data; }],
    ["/api/settings/behavior", (data) => { runtime.behaviorSettings = data; }],
    ["/api/settings/model", (data) => { runtime.modelSettings = data; }],
    ["/api/settings/web-search", (data) => { runtime.webSearchSettings = data; }],
    ["/api/settings/asr", (data) => { runtime.asrSettings = data; }],
    ["/api/messages?limit=" + runtime.pageSize + "&offset=" + runtime.messageOffset, (data) => { runtime.messages = data.messages; }],
    ["/api/tasks?limit=" + runtime.pageSize + "&offset=" + runtime.taskOffset, (data) => { runtime.tasks = data.items; }],
    ["/api/outbox/unknown", (data) => { runtime.unknown = data.items; }],
    ["/api/identity/people?limit=100&offset=0", (data) => { runtime.identityPeople = data.items; runtime.identityPeopleSource = data.source ?? ""; }],
    ["/api/identity/incoming-friend-requests?limit=100&offset=0", (data) => { runtime.identityFriendRequests = [...data.items, ...(data.archived_items ?? [])]; runtime.identityRequestSource = data.source ?? ""; }],
    ["/api/incident-pilot/status", (data) => { runtime.incidentStatus = data; }],
    ["/api/incident-pilot/settings", (data) => { runtime.incidentSettings = data.settings; }],
    ["/api/incidents?limit=100", (data) => { runtime.incidents = data.items; }],
    ["/api/incidents/chat-controls", (data) => { runtime.chatControls = data.items; }],
    ["/api/relationship-pilot/status", (data) => { runtime.relationshipStatus = data; }],
    ["/api/relationship-pilot/settings", (data) => { runtime.relationshipSettings = data; }],
    ["/api/qzone-interactions/status", (data) => { runtime.qzoneInteractionsStatus = data; }],
    ["/api/qzone-interactions/settings", (data) => { runtime.qzoneInteractionsSettings = data; }],
    ["/api/relationships?limit=100&offset=0", (data) => { runtime.relationships = data.items; }],
    ["/api/group-invites?limit=100&offset=0", (data) => { runtime.groupInvites = data.items; runtime.groupInviteDirectEntries = data.directEntries ?? []; }],
    ["/api/group-invites/settings", (data) => { runtime.groupInviteSettings = data; }],
    ["/api/proactive/status", (data) => { runtime.proactiveStatus = data; }],
    ["/api/stickers/settings", (data) => { runtime.stickerLifecycle = data; }],
    ["/api/sticker-collection/settings", (data) => { runtime.stickerCollectionLifecycle = data; }],
    ["/api/memory-consolidation/status", (data) => { runtime.memoryConsolidationStatus = data; }],
    ["/api/multimodal-context/status", (data) => { runtime.multimodalContextStatus = data; }],
  ];
  await Promise.all(routes.map(async ([path, apply]) => {
    const key = path.split("?")[0];
    try {
      const response = await apiFetch(path, { credentials: "same-origin", signal: AbortSignal.timeout(8000) });
      if (!response.ok) throw new Error("读取失败（HTTP " + response.status + "）");
      apply(await response.json());
      delete runtime.apiErrors[key];
    } catch (error) {
      runtime.apiErrors[key] = error instanceof Error ? error.message : "读取失败";
    }
  }));
  // An optional module failure must not blank every other workspace.
  runtime.connected = !runtime.apiErrors["/api/snapshot"];
  listeners.forEach((listener) => listener());
}

export async function updateIncident(
  id: string,
  action: "acknowledge" | "resolve" | "delete" | "notification/retry",
  adminToken: string,
  body: { resolution?: string; confirm?: boolean } = {},
): Promise<string | null> {
  const response = await apiFetch(`/api/incidents/${encodeURIComponent(id)}/${action === "delete" ? "" : action}`.replace(/\/$/, ""), {
    method: action === "delete" ? "DELETE" : "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `操作失败（HTTP ${response.status}）`;
  }
  const result = await response.json().catch(() => ({})) as { settings?: ProactiveSettings };
  if (result.settings) runtime.proactiveSettings = result.settings;
  await refreshRuntime();
  listeners.forEach((listener) => listener());
  return null;
}

export async function updateIncidentLifecycle(
  settings: { enabled: boolean; graduated: boolean },
  adminToken: string,
  confirmGraduation: boolean,
): Promise<string | null> {
  const response = await apiFetch("/api/incident-pilot/lifecycle", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ ...settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `更新异常处理状态失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function updateIncidentOperationalSettings(
  settings: Pick<IncidentSettings, "notifyWarnings" | "duplicateWindowMinutes" | "retentionDays" | "unknownWritesBlockChat">,
  adminToken: string,
): Promise<string | null> {
  const response = await apiFetch("/api/incident-pilot/settings/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存异常处理参数失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function loadRelationshipDetail(uin: string): Promise<void> {
  runtime.relationshipDetailError = "";
  runtime.relationshipEvaluationTrace = null;
  runtime.relationshipEvaluationTraceError = "";
  const response = await apiFetch(`/api/relationships/${encodeURIComponent(uin)}`, { credentials: "same-origin" });
  const result = await response.json().catch(() => ({})) as RelationshipDetail & { error?: string };
  if (!response.ok) {
    runtime.relationshipDetail = null;
    runtime.relationshipDetailError = result.error || `读取失败（HTTP ${response.status}）`;
  } else {
    runtime.relationshipDetail = result;
  }
  listeners.forEach((listener) => listener());
}

export async function loadRelationshipEvaluationTrace(
  uin: string,
  evaluationId: string,
  adminToken: string,
): Promise<void> {
  runtime.relationshipEvaluationTrace = null;
  runtime.relationshipEvaluationTraceError = "";
  const response = await apiFetch(
    `/api/relationships/${encodeURIComponent(uin)}/evaluations/${encodeURIComponent(evaluationId)}/trace`,
    { credentials: "same-origin", headers: { authorization: `Bearer ${adminToken}` } },
  );
  const result = await response.json().catch(() => ({})) as RelationshipEvaluationTrace & { error?: string };
  if (!response.ok) {
    runtime.relationshipEvaluationTraceError = result.error || `读取失败（HTTP ${response.status}）`;
  } else {
    runtime.relationshipEvaluationTrace = result;
  }
  listeners.forEach((listener) => listener());
}

export async function saveChatControl(
  chatKey: string,
  body: { mode: string; reason: string; expectedVersion: number; backlogAction: string; confirm: boolean },
  adminToken: string,
): Promise<string | null> {
  const response = await apiFetch(`/api/incidents/chat-controls/${encodeURIComponent(chatKey)}`, {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function saveQueueSettings(settings: QueueSettings, adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/settings/queue/update", {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  runtime.queueSettings = settings;
  listeners.forEach((listener) => listener());
  return null;
}

export async function saveTimeControlSettings(settings: TimeControlSettings, adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/settings/time-control/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  runtime.timeControlSettings = settings;
  listeners.forEach((listener) => listener());
  return null;
}

export async function saveWebSearchSettings(settings: WebSearchSettings, adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/settings/web-search/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  runtime.webSearchSettings = settings;
  listeners.forEach((listener) => listener());
  return null;
}

export async function saveAsrSettings(settings: AsrSettings, adminToken: string, apiKey: string, clearApiKey: boolean): Promise<string | null> {
  const response = await apiFetch("/api/settings/asr/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings, ...(apiKey ? { api_key: apiKey } : {}), clear_api_key: clearApiKey }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  const refreshed = await apiFetch("/api/settings/asr", { credentials: "same-origin" });
  if (!refreshed.ok) return "设置已保存，但读取最新状态失败；请刷新页面核对";
  runtime.asrSettings = await refreshed.json() as AsrSettingsResponse;
  listeners.forEach((listener) => listener());
  return null;
}

export async function saveRelationshipPilotSettings(settings: RelationshipPilotSettings, adminToken: string, confirmGraduation = false): Promise<string | null> {
  const response = await apiFetch("/api/relationship-pilot/settings/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function saveGroupInviteSettings(settings: GroupInviteSettings, adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/group-invites/settings/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存群邀请设置失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function saveQzoneInteractionsSettings(settings: QzoneInteractionsSettings, adminToken: string, confirmGraduation = false): Promise<string | null> {
  const response = await apiFetch("/api/qzone-interactions/settings/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function saveStickerLifecycle(settings: StickerLifecycle["settings"], adminToken: string, confirmGraduation = false): Promise<string | null> {
  const response = await apiFetch("/api/stickers/settings/update", {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function saveStickerCollectionLifecycle(settings: StickerCollectionLifecycle["settings"], adminToken: string, confirmGraduation = false): Promise<string | null> {
  const response = await apiFetch("/api/sticker-collection/settings/update", {
    method: "PUT",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function saveProactiveSettings(settings: ProactiveSettings, adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/proactive/settings/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ settings }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  const result = await response.json().catch(() => ({})) as { settings?: ProactiveSettings };
  if (result.settings) runtime.proactiveSettings = result.settings;
  await refreshRuntime();
  listeners.forEach((listener) => listener());
  return null;
}

export async function loadProactiveData(adminToken: string): Promise<string | null> {
  runtime.proactiveError = "";
  const headers = { authorization: `Bearer ${adminToken}` };
  const [settingsResponse, wakesResponse] = await Promise.all([
    apiFetch("/api/proactive/settings", { credentials: "same-origin", headers }),
    apiFetch("/api/proactive/wakes?limit=50&offset=0", { credentials: "same-origin", headers }),
  ]);
  const settingsResult = await settingsResponse.json().catch(() => ({})) as ProactiveSettings & { error?: string };
  const wakesResult = await wakesResponse.json().catch(() => ({})) as { items?: ProactiveWake[]; error?: string };
  if (!settingsResponse.ok || !wakesResponse.ok) {
    runtime.proactiveError = settingsResult.error || wakesResult.error || "读取主动消息数据失败";
    listeners.forEach((listener) => listener());
    return runtime.proactiveError;
  }
  runtime.proactiveSettings = settingsResult;
  runtime.proactiveWakes = wakesResult.items ?? [];
  runtime.proactiveLoaded = true;
  listeners.forEach((listener) => listener());
  return null;
}

export async function updateProactiveLifecycle(settings: Pick<ProactiveSettings, "enabled" | "graduated">, adminToken: string, confirmGraduation: boolean): Promise<string | null> {
  const response = await apiFetch("/api/proactive/lifecycle", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ ...settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `操作失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function updateMemoryConsolidationLifecycle(
  settings: Pick<MemoryConsolidationStatus, "enabled" | "graduated">,
  adminToken: string,
  confirmGraduation: boolean,
): Promise<string | null> {
  const response = await apiFetch("/api/memory-consolidation/lifecycle", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ ...settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `操作失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function updateMultimodalContextLifecycle(
  settings: Pick<MultimodalContextStatus, "enabled" | "graduated">,
  adminToken: string,
  confirmGraduation: boolean,
): Promise<string | null> {
  const response = await apiFetch("/api/multimodal-context/lifecycle", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ ...settings, confirmGraduation }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `操作失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function runMemoryConsolidation(
  chatKey: string,
  userIds: string[] | null,
  adminToken: string,
): Promise<string | null> {
  runtime.memoryConsolidationRunError = "";
  runtime.memoryConsolidationRunResult = null;
  const response = await apiFetch("/api/memory-consolidation/run", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ chatKey, ...(userIds ? { userIds } : {}), confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as MemoryConsolidationRunResult & { error?: string; accepted?: boolean };
  if (!response.ok) {
    runtime.memoryConsolidationRunError = result.error || `整理失败（HTTP ${response.status}）`;
    listeners.forEach((listener) => listener());
    return runtime.memoryConsolidationRunError;
  }
  if (!result.accepted) {
    runtime.memoryConsolidationRunResult = result;
    listeners.forEach((listener) => listener());
    return null;
  }
  while (true) {
    await new Promise((resolve) => window.setTimeout(resolve, 1000));
    const statusResponse = await apiFetch("/api/memory-consolidation/status", { credentials: "same-origin" });
    if (!statusResponse.ok) {
      runtime.memoryConsolidationRunError = `读取整理进度失败（HTTP ${statusResponse.status}）`;
      listeners.forEach((listener) => listener());
      return runtime.memoryConsolidationRunError;
    }
    runtime.memoryConsolidationStatus = await statusResponse.json() as MemoryConsolidationStatus;
    listeners.forEach((listener) => listener());
    const current = runtime.memoryConsolidationStatus.manualRun;
    if (current.state !== "running") {
      runtime.memoryConsolidationRunResult = current;
      if (current.state === "cancelled" || current.state === "failed") {
        runtime.memoryConsolidationRunError = current.error || "整理任务已停止；已保存的版本不会回滚";
      }
      listeners.forEach((listener) => listener());
      return runtime.memoryConsolidationRunError || null;
    }
  }
}

export async function loadMemoryConsolidationHistory(
  chatKey: string,
  userId: string,
  adminToken: string,
): Promise<string | null> {
  runtime.memoryConsolidationHistoryError = "";
  const response = await apiFetch("/api/memory-consolidation/history", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ chatKey, userId }),
  });
  const result = await response.json().catch(() => ({})) as MemoryConsolidationHistory & { error?: string };
  if (!response.ok) {
    runtime.memoryConsolidationHistoryError = result.error || `读取版本失败（HTTP ${response.status}）`;
    listeners.forEach((listener) => listener());
    return runtime.memoryConsolidationHistoryError;
  }
  runtime.memoryConsolidationHistory = result;
  listeners.forEach((listener) => listener());
  return null;
}

export async function restoreMemoryConsolidationRevision(
  history: MemoryConsolidationHistory,
  targetRevision: number,
  adminToken: string,
): Promise<string | null> {
  runtime.memoryConsolidationHistoryError = "";
  const response = await apiFetch("/api/memory-consolidation/restore", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({
      chatKey: history.chatKey,
      userId: history.userId,
      targetRevision,
      expectedCurrentRevision: history.currentRevision,
      confirm: true,
    }),
  });
  const result = await response.json().catch(() => ({})) as { revision?: number; error?: string };
  if (!response.ok) {
    runtime.memoryConsolidationHistoryError = result.error || `恢复失败（HTTP ${response.status}）`;
    listeners.forEach((listener) => listener());
    return runtime.memoryConsolidationHistoryError;
  }
  return loadMemoryConsolidationHistory(history.chatKey, history.userId, adminToken);
}

export async function restoreNodeMemoryConsolidationBackup(
  history: MemoryConsolidationHistory,
  backupKey: string,
  adminToken: string,
): Promise<string | null> {
  runtime.memoryConsolidationHistoryError = "";
  const response = await apiFetch("/api/memory-consolidation/restore-node-backup", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({
      chatKey: history.chatKey,
      userId: history.userId,
      backupKey,
      expectedCurrentRevision: history.currentRevision,
      confirm: true,
    }),
  });
  const result = await response.json().catch(() => ({})) as { revision?: number; error?: string };
  if (!response.ok) {
    runtime.memoryConsolidationHistoryError = result.error || `恢复 Node 快照失败（HTTP ${response.status}）`;
    listeners.forEach((listener) => listener());
    return runtime.memoryConsolidationHistoryError;
  }
  return loadMemoryConsolidationHistory(history.chatKey, history.userId, adminToken);
}

export async function loadModelMemoryMutationHistory(
  chatKey: string,
  userId: string,
  adminToken: string,
): Promise<string | null> {
  runtime.modelMemoryMutationHistoryError = "";
  const response = await apiFetch("/api/model-memory/history", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ chatKey, userId }),
  });
  const result = await response.json().catch(() => ({})) as ModelMemoryMutationHistory & { error?: string };
  if (!response.ok) {
    runtime.modelMemoryMutationHistoryError = result.error || `读取变更记录失败（HTTP ${response.status}）`;
    listeners.forEach((listener) => listener());
    return runtime.modelMemoryMutationHistoryError;
  }
  runtime.modelMemoryMutationHistory = result;
  listeners.forEach((listener) => listener());
  return null;
}

export async function restoreModelMemoryRemoval(
  removalOperationId: string,
  chatKey: string,
  userId: string,
  adminToken: string,
): Promise<string | null> {
  runtime.modelMemoryMutationHistoryError = "";
  const response = await apiFetch("/api/model-memory/restore", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({
      removalOperationId,
      restoreOperationId: `admin-restore:${crypto.randomUUID()}`,
      confirm: true,
    }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) {
    runtime.modelMemoryMutationHistoryError = result.error || `恢复失败（HTTP ${response.status}）`;
    listeners.forEach((listener) => listener());
    return runtime.modelMemoryMutationHistoryError;
  }
  return loadModelMemoryMutationHistory(chatKey, userId, adminToken);
}

export async function cancelProactiveWake(id: string, adminToken: string): Promise<string | null> {
  const response = await apiFetch(`/api/proactive/wakes/${encodeURIComponent(id)}/cancel`, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({ confirm: true }),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `取消失败（HTTP ${response.status}）`;
  }
  const reloadError = await loadProactiveData(adminToken);
  await refreshRuntime();
  if (reloadError) return reloadError;
  return null;
}

export async function saveBehaviorSettings(settings: BehaviorSettings, adminToken: string): Promise<string | null> {
  const response = await apiFetch("/api/settings/behavior/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify(settings),
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `保存失败（HTTP ${response.status}）`;
  }
  runtime.behaviorSettings = settings;
  listeners.forEach((listener) => listener());
  return null;
}

export async function saveModelSettings(
  settings: ModelSettings,
  adminToken: string,
  apiKey: string,
  clearApiKey: boolean,
  fallbackApiKey: string,
  clearFallbackApiKey: boolean,
  visionApiKey: string,
  clearVisionApiKey: boolean,
): Promise<{ error: string } | null> {
  const response = await apiFetch("/api/settings/model/update", {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json", authorization: `Bearer ${adminToken}` },
    body: JSON.stringify({
      public: settings,
      api_key: apiKey || undefined,
      clear_api_key: clearApiKey,
      fallback_api_key: fallbackApiKey || undefined,
      clear_fallback_api_key: clearFallbackApiKey,
      vision_api_key: visionApiKey || undefined,
      clear_vision_api_key: clearVisionApiKey,
    }),
  });
  const result = await response.json().catch(() => ({})) as {
    error?: string;
    settings?: ModelSettings;
    api_key_configured?: boolean;
    fallback_api_key_configured?: boolean;
    vision_api_key_configured?: boolean;
  };
  if (!response.ok) return { error: result.error || `保存失败（HTTP ${response.status}）` };
  runtime.modelSettings = {
    settings: result.settings ?? settings,
    api_key_configured: result.api_key_configured ?? false,
    fallback_api_key_configured: result.fallback_api_key_configured ?? false,
    vision_api_key_configured: result.vision_api_key_configured ?? false,
  };
  listeners.forEach((listener) => listener());
  return null;
}

export async function cancelQueuedTask(id: number, adminToken: string): Promise<string | null> {
  const response = await apiFetch(`/api/tasks/inbox-${encodeURIComponent(String(id))}/cancel`, {
    method: "POST",
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  if (!response.ok) {
    const result = await response.json().catch(() => ({})) as { error?: string };
    return result.error || `取消失败（HTTP ${response.status}）`;
  }
  await refreshRuntime();
  return null;
}

export async function reconcileUnknownOutbox(
  id: string,
  outcome: "sent" | "failed",
  adminToken: string,
): Promise<string | null> {
  try {
    const response = await apiFetch(`/api/outbox/${encodeURIComponent(id)}/reconcile`, {
      method: "POST",
      credentials: "same-origin",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${adminToken}`,
      },
      body: JSON.stringify({ outcome }),
    });
    if (!response.ok) {
      const result = await response.json().catch(() => ({})) as { error?: string };
      return result.error || `对账失败（HTTP ${response.status}）`;
    }
    await refreshRuntime();
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : "对账请求失败";
  }
}

export async function decideIncomingFriendRequest(
  id: string,
  decision: "approve" | "reject",
  remark: string,
  adminToken: string,
): Promise<{ note: string } | { error: string }> {
  const response = await apiFetch(`/api/identity/incoming-friend-requests/${encodeURIComponent(id)}/decision`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ decision, remark }),
  });
  const result = await response.json().catch(() => ({})) as { note?: string; error?: string };
  if (!response.ok) return { error: result.error || `处理失败（HTTP ${response.status}）` };
  return { note: result.note || "申请状态已更新" };
}

export async function updateIdentityProfile(
  uin: string,
  update: { primaryName?: string; isFriend?: boolean; profile?: Record<string, unknown> },
  adminToken: string,
): Promise<string | null> {
  const response = await apiFetch(`/api/identity/people/${encodeURIComponent(uin)}/profile`, {
    method: "PUT",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ ...update, confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as { note?: string; error?: string };
  if (!response.ok) return result.error || `保存失败（HTTP ${response.status}）`;
  await refreshRuntime();
  return null;
}

export async function mergeIdentityPerson(sourceUin: string, targetUin: string, adminToken: string): Promise<string | null> {
  const response = await apiFetch(`/api/identity/people/${encodeURIComponent(sourceUin)}/merge`, {
    method: "POST",
    credentials: "same-origin",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${adminToken}`,
    },
    body: JSON.stringify({ targetUin, confirm: true }),
  });
  const result = await response.json().catch(() => ({})) as { error?: string };
  if (!response.ok) return result.error || `合并失败（HTTP ${response.status}）`;
  await refreshRuntime();
  return null;
}

export async function retryPrivateWhitelist(id: string, adminToken: string): Promise<{ request: LegacyIdentityRecord } | { error: string }> {
  const response = await apiFetch(`/api/identity/incoming-friend-requests/${encodeURIComponent(id)}/whitelist/retry`, {
    method: "POST",
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  const result = await response.json().catch(() => ({})) as { request?: LegacyIdentityRecord; error?: string };
  if (!response.ok) return { error: result.error || `同步失败（HTTP ${response.status}）` };
  return { request: result.request ?? {} };
}

export async function retryFriendRequestNotification(id: string, adminToken: string): Promise<{ request: LegacyIdentityRecord } | { error: string }> {
  const response = await apiFetch(`/api/identity/incoming-friend-requests/${encodeURIComponent(id)}/notification/retry`, {
    method: "POST",
    credentials: "same-origin",
    headers: { authorization: `Bearer ${adminToken}` },
  });
  const result = await response.json().catch(() => ({})) as { request?: LegacyIdentityRecord; error?: string };
  if (!response.ok) return { error: result.error || `通知失败（HTTP ${response.status}）` };
  return { request: result.request ?? {} };
}

export function startRuntime(): () => void {
  void refreshRuntime();
  const controller = new AbortController();
  let debounce: ReturnType<typeof setTimeout>;
  let reconnect: ReturnType<typeof setTimeout>;
  const labels: Record<string, string> = {
    hello: "实时连接已建立", "snapshot-required": "正在补齐运行快照",
    "message.received": "收到新消息", "onebot.connected": "NapCat 已连接", "onebot.disconnected": "NapCat 连接已断开",
    "outbox.reconciled": "外发对账已更新", "incident.created": "新增异常记录", "incident.updated": "异常记录已更新",
    "memory-consolidation.manual.completed": "人物记忆整理完成", "group-invite.request": "收到群邀请",
    "daily-moments.generation.completed": "每日内容草稿生成完成",
  };
  const update = (name: string) => {
    runtime.events.unshift({ at: Date.now(), label: labels[name] ?? (name.startsWith("settings.") ? "管理设置已更新" : "运行记录已更新") });
    runtime.events.splice(100);
    clearTimeout(debounce);
    debounce = setTimeout(() => void refreshRuntime(), 150);
  };
  // Fetch streaming supports Authorization without putting a token in the URL or a JS-readable cookie.
  const listen = async () => {
    while (!controller.signal.aborted) {
      try {
        const response = await apiFetch("/api/events", { signal: controller.signal });
        if (!response.ok || !response.body) throw new Error("events unavailable");
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        try {
          while (!controller.signal.aborted) {
            const { value, done } = await reader.read();
            if (done) break;
            buffer += decoder.decode(value, { stream: true });
            const frames = buffer.split(/\r?\n\r?\n/);
            buffer = frames.pop() ?? "";
            if (buffer.length > 65536) throw new Error("event too large");
            for (const frame of frames) {
              const name = frame.match(/^event:\s*(.+)$/m)?.[1]?.trim();
              if (name) update(name); // Do not retain payloads, QQ contents or credentials in the event list.
            }
          }
        } finally { await reader.cancel().catch(() => {}); }
      } catch {
        if (!controller.signal.aborted) { runtime.connected = false; listeners.forEach((listener) => listener()); }
      }
      if (!controller.signal.aborted) await new Promise<void>((resolve) => {
        const done = () => { controller.signal.removeEventListener("abort", done); resolve(); };
        reconnect = setTimeout(done, 2000);
        controller.signal.addEventListener("abort", done, { once: true });
      });
    }
  };
  void listen();
  const poll = setInterval(() => { if (!document.hidden) void refreshRuntime(); }, 2000);
  return () => { controller.abort(); clearTimeout(debounce); clearTimeout(reconnect); clearInterval(poll); };
}
