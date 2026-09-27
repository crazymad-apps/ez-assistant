import { compatibilityHeaders, type RuntimeCommand, type RuntimeCommandResult } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";
import { createServer } from "node:http";
import { readFile, unlink, writeFile } from "node:fs/promises";
import { join } from "node:path";

test("F02 delivers only declared files, previews/downloads, reloads and withdraws missing files", async ({ page }, info) => {
  test.setTimeout(60_000);
  let sequence = 0;
  const server = createServer(async (request, response) => {
    if (request.method === "GET") {
      response.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ object: "list", data: [{ id: "artifact-fixture", object: "model" }] }));
      return;
    }
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw) as { messages: { role: string; content: string | { text?: string }[] }[] };
    const messages = body.messages.map((message) => ({ ...message,
      text: typeof message.content === "string" ? message.content : message.content?.map((part) => part.text ?? "").join("\n") ?? "",
    }));
    // Runtime 可能在用户输入之后投影 MCP 等上下文；只识别本测试的用户指令。
    const index = messages.findLastIndex((message) => message.role === "user" && message.text.startsWith("F02 "));
    const text = messages[index]!.text;
    const done = body.messages.slice(index + 1).some((message) => message.role === "tool");
    const id = `artifact-${++sequence}`;
    const delta = done ? { role: "assistant", content: "产物清单已更新" } : { role: "assistant", tool_calls: [{ index: 0, id, type: "function", function: { name: "declare_artifact", arguments: JSON.stringify({ action: text.includes("WITHDRAW") ? "withdraw" : "declare", path: "final.txt", title: text.includes("UPDATE") ? "修订报告" : "最终报告" }) } }] };
    const first = { id, model: "artifact-fixture", choices: [{ index: 0, delta, finish_reason: null }] };
    const last = { id, model: "artifact-fixture", choices: [{ index: 0, delta: {}, finish_reason: done ? "stop" : "tool_calls" }], usage: { prompt_tokens: 20, completion_tokens: 5, total_tokens: 25 } };
    response.writeHead(200, { "content-type": "text/event-stream" }).end(`data: ${JSON.stringify(first)}\n\ndata: ${JSON.stringify(last)}\n\ndata: [DONE]\n\n`);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("fixture port unavailable");
  try {
    const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
    const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
    async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
      const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers, body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
      const body = await response.json();
      expect(response.status, JSON.stringify(body)).toBe(200);
      return body.result.payload.payload;
    }
    const provider = await command("create_provider", { connection: { display_name: "F02 fixture", provider_type: "local", endpoint: `http://127.0.0.1:${address.port}/v1`, protocol_preference: "chat_completions", models_path: "/v1/models", discovery_format: "openai" }, credential: { mode: "replace", value: "fixture-only" } });
    await command("refresh_provider_models", { provider_instance_id: provider.provider_instance_id });
    const selection = { provider_instance_id: provider.provider_instance_id, model_id: "artifact-fixture" };
    await command("save_model_fixed_config", { selection, parameters: {
      context_window_tokens: { state: "known", value: 16384 }, max_output_tokens: { state: "known", value: 4096 },
      max_input_tokens: { state: "unknown" }, reasoning_max_input_tokens: { state: "unknown" }, reasoning_max_output_tokens: { state: "unknown" },
      streaming: "supported", image_input: "unsupported", tool_calls: "supported", reasoning: "unsupported",
      tool_choice: { auto: "supported", none: "supported", required: "supported", named: "supported" },
      tool_image_projection: "unsupported", reasoning_mode: "unsupported", reasoning_efforts: {}, default_reasoning_effort: null,
    } });
    const directory = process.env.EZ_ASSISTANT_E2E_NEW_WORKSPACE!;
    await writeFile(join(directory, "final.txt"), "F02 FINAL DELIVERY");
    await writeFile(join(directory, "helper.py"), "print('helper')");
    await writeFile(join(directory, "cache.tmp"), "temporary");
    const workspace = (await command("register_workspace", { label: "F02 产物", primary_directory: directory, additional_directories: [] })).workspace;
    const session_id = (await command("create_session", { title: "F02 交付测试", model_selection: selection, workspace_id: workspace.workspace_id })).session.session_id;
    const login = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
    const { token } = await login.json();
    await page.emulateMedia({ reducedMotion: "reduce" });
    await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
    await page.getByText("F02 交付测试", { exact: true }).first().click();
    async function submit(message: string) {
      const { run } = await command("submit_input", { session_id, message, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
      await expect.poll(async () => (await command("get_run", { session_id, run_id: run.run_id })).run.status).toBe("completed");
    }
    await submit("F02 DECLARE");
    const panel = page.getByRole("complementary", { name: "资源栏" });
    await expect(panel.getByRole("button", { name: "最终报告", exact: true })).toBeEnabled();
    await expect(panel.getByText("helper.py", { exact: true })).toHaveCount(0);
    await expect(panel.getByText("cache.tmp", { exact: true })).toHaveCount(0);
    await page.screenshot({ path: info.outputPath("artifacts-ready.png") });
    const download_event = page.waitForEvent("download");
    await panel.getByRole("button", { name: "下载 最终报告", exact: true }).click();
    const download = await download_event;
    expect(await readFile((await download.path())!, "utf8")).toBe("F02 FINAL DELIVERY");
    await panel.getByRole("button", { name: "最终报告", exact: true }).click();
    await expect(panel.getByRole("tab", { name: "final.txt", exact: true })).toBeVisible();
    await expect(panel.getByText("F02 FINAL DELIVERY", { exact: false }).first()).toBeVisible();
    await panel.getByRole("tab", { name: "当前上下文", exact: true }).click();
    await submit("F02 UPDATE");
    expect((await command("get_session_view", { session_id })).snapshot.value.artifacts[0]?.title).toBe("修订报告");
    await expect(panel.getByRole("button", { name: "修订报告", exact: true })).toBeEnabled();
    expect((await command("get_session_view", { session_id })).snapshot.value.artifacts).toHaveLength(1);
    await page.reload();
    await page.getByText("F02 交付测试", { exact: true }).first().click();
    await expect(panel.getByRole("button", { name: "修订报告", exact: true })).toBeEnabled();
    await unlink(join(directory, "final.txt"));
    await panel.getByRole("button", { name: "刷新产物状态", exact: true }).click();
    await expect(panel.getByText(/文件不可用/)).toBeVisible();
    await expect(panel.getByRole("button", { name: "下载 修订报告", exact: true })).toBeDisabled();
    await page.screenshot({ path: info.outputPath("artifacts-missing.png") });
    await submit("F02 WITHDRAW");
    await expect(panel.getByText("尚无产物", { exact: true })).toBeVisible();
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});
