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
  attempts: number;
  error?: string;
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
  api_token_configured: boolean;
  onebot_token_configured: boolean;
};

export const runtime = {
  snapshot: null as RuntimeSnapshot | null,
  config: null as RuntimeConfig | null,
  messages: [] as RuntimeMessage[],
  unknown: [] as RuntimeUnknown[],
  connected: false,
};

const listeners = new Set<() => void>();

export function subscribeRuntime(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export async function refreshRuntime(): Promise<void> {
  try {
    const [snapshotResponse, configResponse, messagesResponse, unknownResponse] = await Promise.all([
      fetch("/api/snapshot", { credentials: "same-origin" }),
      fetch("/api/config", { credentials: "same-origin" }),
      fetch("/api/messages?limit=8&offset=0", { credentials: "same-origin" }),
      fetch("/api/outbox/unknown", { credentials: "same-origin" }),
    ]);
    if (!snapshotResponse.ok || !configResponse.ok || !messagesResponse.ok || !unknownResponse.ok) throw new Error("runtime API unavailable");
    runtime.snapshot = (await snapshotResponse.json()) as RuntimeSnapshot;
    runtime.config = (await configResponse.json()) as RuntimeConfig;
    runtime.messages = ((await messagesResponse.json()) as { messages: RuntimeMessage[] }).messages;
    runtime.unknown = ((await unknownResponse.json()) as { items: RuntimeUnknown[] }).items;
    runtime.connected = true;
  } catch {
    runtime.connected = false;
  }
  listeners.forEach((listener) => listener());
}

export function startRuntime(): () => void {
  void refreshRuntime();
  const events = new EventSource("/api/events", { withCredentials: true });
  events.onmessage = () => void refreshRuntime();
  events.addEventListener("snapshot-required", () => void refreshRuntime());
  events.onerror = () => { runtime.connected = false; listeners.forEach((listener) => listener()); };
  return () => events.close();
}
