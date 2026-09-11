import test from "node:test";
import assert from "node:assert/strict";
import https from "node:https";
import * as tls from "node:tls";
import { mkdtemp, readFile, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { request } from "../dist/host/http.js";

test("HTTPS 拒绝未受信证书与关闭验证的环境变量，显式附加 CA 可连接", async () => {
  const root = await mkdtemp(join(tmpdir(), "ez-client-tls-"));
  let server;
  try {
    const config = join(root, "openssl.cnf"), cert = join(root, "cert.pem"), key = join(root, "key.pem");
    await writeFile(config, "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=isolated-client-test\n[ext]\nsubjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:TRUE\n");
    const generated = spawnSync("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config", config, "-keyout", key, "-out", cert], { encoding: "utf8" });
    assert.equal(generated.status, 0, generated.stderr);
    let requests = 0;
    server = https.createServer({ key: await readFile(key), cert: await readFile(cert) }, (_, response) => {
      requests++; response.setHeader("content-type", "application/json"); response.end('{"status":"ready"}');
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const target = { address: `https://127.0.0.1:${server.address().port}`, access_token: "test-only" };
    await assert.rejects(request(target, "/health"), { code: "connection_unknown" });
    const child = async (env) => {
      const source = `import {request} from ${JSON.stringify(new URL("../dist/host/http.js", import.meta.url).href)}; await request(${JSON.stringify(target)}, "/health");`;
      return new Promise((resolve, reject) => {
        const process_ = spawn(process.execPath, ["--input-type=module", "-e", source], { env: { ...process.env, ...env }, stdio: "ignore" });
        process_.on("error", reject); process_.on("close", resolve);
      });
    };
    assert.notEqual(await child({ NODE_EXTRA_CA_CERTS: "", NODE_TLS_REJECT_UNAUTHORIZED: "0" }), 0);
    assert.equal(requests, 0);
    assert.equal(await child({ NODE_EXTRA_CA_CERTS: cert, NODE_TLS_REJECT_UNAUTHORIZED: "1" }), 0);
    assert.equal(requests, 1);
    if (process.platform === "linux") {
      // 在进程私有 OpenSSL 信任路径验证系统 CA，无需修改机器的证书存储或启动参数。
      const code = await child({ NODE_EXTRA_CA_CERTS: "", SSL_CERT_FILE: cert, SSL_CERT_DIR: root, NODE_TLS_REJECT_UNAUTHORIZED: "1" });
      if (typeof tls.getCACertificates === "function") {
        assert.equal(code, 0);
        assert.equal(requests, 2);
      } else {
        // 旧 Node 不自动导入系统 CA；仅配置该路径不能绕过默认信任。
        assert.notEqual(code, 0);
        assert.equal(requests, 1);
      }
    }
  } finally {
    if (server) { server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); }
    await rm(root, { recursive: true, force: true });
  }
});
