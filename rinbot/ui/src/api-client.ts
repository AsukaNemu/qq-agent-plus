let sessionToken = "";
export const getAdminToken = () => sessionToken;

export async function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (sessionToken && !headers.has("authorization")) headers.set("authorization", `Bearer ${sessionToken}`);
  if (init.method && !["GET", "HEAD"].includes(init.method.toUpperCase()) && typeof document !== "undefined") {
    document.dispatchEvent(new Event("rinbot-write-start"));
  }
  try {
    return await fetch(path, { ...init, headers, credentials: "same-origin", signal: init.signal ?? AbortSignal.timeout(120000) });
  } catch {
    return new Response(JSON.stringify({ error: "请求中断，请检查连接后核对结果；不会自动重试。" }), { status: 503, headers: { "content-type": "application/json" } });
  }
}

export async function authenticateSession(token: string): Promise<boolean> {
  if (!token.trim()) return false;
  const response = await apiFetch("/api/snapshot", { headers: { authorization: `Bearer ${token.trim()}` }, signal: AbortSignal.timeout(8000) });
  if (!response.ok) return false;
  sessionToken = token.trim();
  return true;
}
