import "./rinbot-bridge.css";

type Snapshot = {
  status?: { core_ready?: boolean; onebot_connected?: boolean };
  counts?: { messages?: number; queued_inbox?: number; unknown_outbox?: number };
};

const root = document.createElement("aside");
root.className = "rinbot-bridge";
root.dataset.state = "loading";
root.setAttribute("aria-live", "polite");
root.innerHTML = "<strong><i></i>RInBot / CONNECTING</strong><small>Rust control core</small>";
document.addEventListener("DOMContentLoaded", () => {
  const title = document.querySelector<HTMLElement>(".brand");
  (title ?? document.body).append(root);
}, { once: true });

function paint(snapshot: Snapshot): void {
  const status = snapshot.status ?? {};
  const counts = snapshot.counts ?? {};
  const online = Boolean(status.core_ready);
  root.dataset.state = online ? "online" : "offline";
  root.innerHTML = `<strong><i></i>RInBot / ${online ? "CORE READY" : "OFFLINE"}</strong><small>OneBot: ${status.onebot_connected ? "connected" : "observe-only"} · messages: ${counts.messages ?? 0}</small>`;
}

async function refresh(): Promise<void> {
  try {
    const response = await fetch("/api/snapshot", { credentials: "same-origin" });
    if (!response.ok) throw new Error(String(response.status));
    paint((await response.json()) as Snapshot);
  } catch {
    root.dataset.state = "offline";
    root.innerHTML = "<strong><i></i>RInBot / OFFLINE</strong><small>页面仍可浏览，等待 Rust API</small>";
  }
}

void refresh();
const events = new EventSource("/api/events", { withCredentials: true });
events.addEventListener("message.received", () => void refresh());
events.addEventListener("snapshot-required", () => void refresh());
events.onerror = () => { root.dataset.state = "offline"; };
