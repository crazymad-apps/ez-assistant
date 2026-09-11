import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { mkdtemp, mkdir, writeFile, readFile, chmod, rm, symlink, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { HostControl } from "../dist/host/control.js";
import { canonicalHome, readDiscovery } from "../dist/host/discovery.js";
import { request } from "../dist/host/http.js";
import { currentCompatibility, checkCompatibility } from "@ez-assistant/protocol/node";

const caps = { runtime_version: "0.25.2", min_compatible_version: "0.25.2", features: ["host_access", "startup_diagnostics"], sse: true };
const access = { revision: "initial", password_configured: true, configuration: { port: 7240, scheme: "http", remote_enabled: false, server_names: [], tls_certificate: null, tls_private_key: null }, restart_required: false, listener_state: "listening", error: null };
async function fixture(handler = () => undefined) {
  const root = await mkdtemp(join(tmpdir(), "ez-client-http-"));
  await chmod(root, 0o700); await mkdir(join(root, "run"), { mode: 0o700 });
  const requests = [];
  const server = http.createServer(async (req, res) => {
    let data = ""; for await (const bytes of req) data += bytes;
    const row = { path: req.url, headers: req.headers, body: data ? JSON.parse(data) : null }; requests.push(row);
    const override = await handler(row);
    if (override?.hang) return;
    let body = override?.body ?? (req.url === "/capabilities" ? caps : req.url === "/health" ? { status: "ready" } : { request_id: row.body.request_id, result: { scope: "host_access", payload: access } });
    res.writeHead(override?.status ?? 200, { "content-type": "application/json", ...override?.headers }); res.end(JSON.stringify(body));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const discovery = { address: `http://127.0.0.1:${server.address().port}`, pid: process.pid, access_token: "a".repeat(43), instance_id: "original" };
  const path = join(root, "run/runtime.json"); await writeFile(path, JSON.stringify(discovery), { mode: 0o600 });
  return { root, path, discovery, requests, host: new HostControl(root, new AbortController().signal), async close() { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); await rm(root, { recursive: true }); } };
}
test("软件版本不兼容在任何业务命令前拒绝", async () => {
  for (const versions of [{ runtime_version: "0.25.1", min_compatible_version: "0.25.1" }, { runtime_version: "0.25.3", min_compatible_version: "0.25.3" }, { runtime_version: "0.25.2", min_compatible_version: null }]) {
    const f = await fixture((r) => r.path === "/capabilities" ? { body: { ...caps, ...versions } } : undefined);
    try { await assert.rejects(f.host.start(), { code: "incompatible" }); assert.equal(f.requests.length, 1); assert.equal(f.requests[0].path, "/capabilities"); }
    finally { await f.close(); }
  }
});
test("当前及兼容较新 Host 被复用，诊断状态不伪装就绪", async () => {
  const f = await fixture((r) => r.path === "/capabilities" ? { body: { ...caps, runtime_version: "0.25.3" } } : undefined);
  try { const result = await f.host.start(); assert.equal(result.reused, true); assert.equal(result.target.health.status, "ready"); assert.ok(f.requests.every((r) => r.headers["x-ez-client-version"] === "0.25.2")); }
  finally { await f.close(); }
  const bad = await fixture((r) => r.path === "/health" ? { body: { status: "unavailable", error: "database_host_too_old", min_compatible_host_version: "0.25.3" } } : undefined);
  try { assert.equal((await bad.host.status()).health.status, "unavailable"); await assert.rejects(bad.host.start(), { code: "startup_failed" }); await assert.rejects(bad.host.configuration(), { code: "not_ready" }); }
  finally { await bad.close(); }
});
test("停止固定原凭据，发现后继者后不再补发 shutdown", async () => {
  let f;
  f = await fixture(async (r) => {
    if (r.body?.command?.payload?.type === "shutdown_runtime") {
      await writeFile(f.path, JSON.stringify({ ...f.discovery, instance_id: "successor", access_token: "b".repeat(43) }));
      return { body: { request_id: r.body.request_id, result: { scope: "runtime", payload: { type: "shutdown_runtime", payload: { lifecycle: "shutting_down" } } } } };
    }
  });
  try { await assert.rejects(f.host.stop(), { code: "instance_changed" }); const writes = f.requests.filter((r) => r.body); assert.equal(writes.length, 1); assert.equal(writes[0].headers.authorization, `Bearer ${f.discovery.access_token}`); }
  finally { await f.close(); }
});
test("重启来源缺失在停止前拒绝", async () => {
  const f = await fixture();
  try { await assert.rejects(f.host.restart(), { code: "source_unavailable" }); assert.ok(f.requests.every((r) => !r.body)); }
  finally { await f.close(); }
});
test("配置冲突不重试；已保存但回读失败报告提交状态未知", async () => {
  let writeCount = 0;
  const f = await fixture((r) => {
    if (r.body?.command?.payload?.type === "configure") { writeCount++; return { status: 409, body: { error: { code: "configuration_conflict" } } }; }
  });
  try { const session = await f.host.configuration(); await assert.rejects(session.save({ type: "configure", payload: { expected_revision: "initial", configuration: access.configuration } }), { code: "configuration_conflict" }); assert.equal(writeCount, 1); }
  finally { await f.close(); }
  let saved = false;
  const g = await fixture((r) => {
    const type = r.body?.command?.payload?.type;
    if (type === "set_password") { saved = true; return { body: { request_id: r.body.request_id, result: { scope: "host_access", payload: { ...access, revision: "saved" } } } }; }
    if (type === "get_status" && saved) return { status: 503, body: { error: { code: "configuration_unavailable" } } };
  });
  try { const session = await g.host.configuration(); await assert.rejects(session.save({ type: "set_password", payload: { expected_revision: "initial", password: "test-only" } }), { code: "committed_unknown" }); assert.equal(g.requests.filter((r) => r.body?.command?.payload?.type === "set_password").length, 1); }
  finally { await g.close(); }
});
test("重定向不转发凭据，取消只关闭当前等待", async () => {
  const f = await fixture(() => ({ status: 302, headers: { location: "http://127.0.0.1:1/" }, body: {} }));
  try { await assert.rejects(request(f.discovery, "/health"), { code: "redirect" }); assert.equal(f.requests.length, 1); }
  finally { await f.close(); }
  const g = await fixture(() => ({ hang: true }));
  try { const abort = new AbortController(); const pending = request(g.discovery, "/health", undefined, abort.signal); setTimeout(() => abort.abort(), 30); await assert.rejects(pending); assert.equal(g.requests.length, 1); }
  finally { await g.close(); }
});
test("发现文件权限和 symlink 拒绝，Home 别名规范化且不创建后缀", async () => {
  const f = await fixture();
  try {
    await chmod(f.path, 0o644); await assert.rejects(readDiscovery(f.root), { code: "discovery_invalid" });
    await chmod(f.path, 0o600); const saved = await readFile(f.path); await rm(f.path); await writeFile(join(f.root, "other"), saved, { mode: 0o600 }); await symlink(join(f.root, "other"), f.path);
    await assert.rejects(readDiscovery(f.root), { code: "discovery_invalid" });
    await symlink(f.root, join(f.root, "alias"));
    assert.equal(await canonicalHome(join(f.root, "alias", "missing")), join(await realpath(f.root), "missing"));
  } finally { await f.close(); }
});
test("Node 编译投影保留共享兼容判定", () => { assert.equal(checkCompatibility(currentCompatibility(), currentCompatibility()), null); });

test("密码提交中取消仍完成回读，且只提交一次", async () => {
  const abort = new AbortController(); let saved = false;
  const f = await fixture((r) => {
    if (r.body?.command?.payload?.type === "set_password") { saved = true; abort.abort(); }
    if (r.body) return { body: { request_id: r.body.request_id, result: { scope: "host_access", payload: { ...access, revision: saved ? "saved" : "initial" } } } };
  });
  try {
    const session = await new HostControl(f.root, abort.signal).configuration();
    const result = await session.save({ type: "set_password", payload: { expected_revision: "initial", password: "test-only" } });
    assert.equal(result.revision, "saved"); assert.equal(abort.signal.aborted, true);
    assert.equal(f.requests.filter((r) => r.body?.command?.payload?.type === "set_password").length, 1);
  } finally { await f.close(); }
});
