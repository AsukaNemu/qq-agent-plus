import assert from "node:assert/strict";
import test from "node:test";
import { apiFetch, authenticateSession, getAdminToken } from "../src/api-client.ts";
import { loadArchivePage, refreshRuntime, runtime } from "../src/rinbot-runtime.ts";
import { parseList } from "../src/policy-forms.ts";

test("QQ lists accept Chinese punctuation and preserve string identifiers", () => {
  assert.deepEqual(parseList("001，002\n001;003；004"), ["001", "002", "003", "004"]);
});

test("administrator authentication is explicit, memory-only and attached to reads", async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let seen = "";
  globalThis.fetch = async (_path, init) => {
    seen = new Headers(init.headers).get("authorization");
    return new Response("{}", { status: seen === "Bearer test-only" ? 200 : 401 });
  };
  assert.equal(await authenticateSession("wrong"), false);
  assert.equal(getAdminToken(), "");
  assert.equal(await authenticateSession("test-only"), true);
  await apiFetch("/api/settings/queue");
  assert.equal(seen, "Bearer test-only");
});

test("network failures have a bounded error and never replay writes", async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let calls = 0;
  globalThis.fetch = async () => { calls++; throw new Error("offline"); };
  const response = await apiFetch("/api/outbox/id/reconcile", { method: "POST" });
  assert.equal(response.status, 503);
  assert.match((await response.json()).error, /不会自动重试/);
  assert.equal(calls, 1);
});

test("one optional API failure does not hide healthy pages or erase last data", async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  const previous = { settings: { model: "retained" } };
  runtime.modelSettings = previous;
  globalThis.fetch = async (path) => {
    if (path === "/api/settings/model") return new Response("{}", { status: 500 });
    return new Response(JSON.stringify(path === "/api/snapshot" ? { status: { core_ready: true }, counts: {} } : { items: [], messages: [], settings: {} }));
  };
  await refreshRuntime();
  assert.equal(runtime.connected, true);
  assert.equal(runtime.modelSettings, previous);
  assert.match(runtime.apiErrors["/api/settings/model"], /500/);
  assert.deepEqual(runtime.messages, []);
});

test("message pagination requests the server offset and preserves data on failure", async (t) => {
  const original = globalThis.fetch;
  t.after(() => { globalThis.fetch = original; });
  let requested;
  globalThis.fetch = async (path) => { requested = path; return new Response(JSON.stringify({ messages: [{ text: "page two" }] })); };
  await loadArchivePage("messages", 50);
  assert.equal(requested, "/api/messages?limit=50&offset=50");
  assert.equal(runtime.messageOffset, 50);
  const previous = runtime.messages;
  globalThis.fetch = async () => new Response("{}", { status: 503 });
  await loadArchivePage("messages", 100);
  assert.equal(runtime.messageOffset, 50);
  assert.equal(runtime.messages, previous);
  assert.equal(runtime.pageLoading, false);
});
