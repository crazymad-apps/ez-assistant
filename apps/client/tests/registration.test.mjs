import { createServer } from "node:http";
import { once } from "node:events";
import assert from "node:assert/strict";
import { test } from "node:test";
import { enterpriseLogin, registration } from "../dist/host/registration.js";

test("登记申请同时传用户身份与独立本机证明，查询无需用户登录", async () => {
  const requests = [];
  const server = createServer(async (req, res) => {
    let body = "";
    for await (const chunk of req) body += chunk;
    requests.push({ headers: req.headers, body: JSON.parse(body) });
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify({ device: null, error: null, intent: null }));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const target = {
    address: `http://127.0.0.1:${server.address().port}`,
    access_token: "native-proof",
    instance_id: "instance",
  };
  try {
    await registration(target, { type: "apply", payload: { name: "设备" } }, undefined, "user-token");
    await registration(target, { type: "get_status" });
    assert.equal(requests[0].headers.authorization, "Bearer user-token");
    assert.equal(requests[0].headers["x-ez-host-bootstrap"], "native-proof");
    assert.equal(requests[1].headers.authorization, "Bearer native-proof");
    assert.equal(requests[1].headers["x-ez-host-bootstrap"], undefined);
    assert.deepEqual(requests[1].body, { type: "get_status" });
  } finally {
    server.close();
    await once(server, "close");
  }
});

test("登录返回其他实例时拒绝继续申请", async () => {
  const server = createServer((_req, res) => {
    res.end(JSON.stringify({ instance_id: "changed", mode: "enterprise", token: "user-token" }));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const target = {
    address: `http://127.0.0.1:${server.address().port}`,
    access_token: "native-proof",
    instance_id: "original",
  };
  try {
    await assert.rejects(enterpriseLogin(target, "test", "test1234", new AbortController().signal), {
      code: "invalid_response",
    });
  } finally {
    server.close();
    await once(server, "close");
  }
});
