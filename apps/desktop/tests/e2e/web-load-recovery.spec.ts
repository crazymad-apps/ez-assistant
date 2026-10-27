import { type RuntimeCommand, type RuntimeCommandResult, compatibilityHeaders } from "@ez-assistant/protocol";
import { expect, test } from "@playwright/test";

test("formal Web bounds reads, isolates selection, and recovers an SSE gap during another session's output", async ({ page }) => {
  test.setTimeout(90_000);
  page.setDefaultTimeout(10_000);
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const host = JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!) as { base_url: string; access_token: string };
  const headers = { ...compatibilityHeaders(), Authorization: `Bearer ${host.access_token}`, "Content-Type": "application/json" };
  async function command<T extends RuntimeCommand["type"]>(type: T, payload: object = {}): Promise<Extract<RuntimeCommandResult, { type: T }>["payload"]> {
    const response = await fetch(`${host.base_url}/commands`, { method: "POST", headers,
      body: JSON.stringify({ request_id: crypto.randomUUID(), command: { scope: "runtime", payload: { type, payload } } }) });
    const body = await response.json();
    expect(response.status, JSON.stringify(body)).toBe(200);
    return body.result.payload.payload;
  }
  const app = (await command("get_application_snapshot")).snapshot.value;
  const sessions = [];
  for (const [title, message] of [["F02 加载 A", "FIRST_CASE"], ["F02 加载 B", "QUEUED_CASE"]]) {
    const session = (await command("create_session", { title, workspace_id: app.workspaces[0].workspace_id })).session;
    const accepted = await command("submit_input", { session_id: session.session_id, message, mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
    await expect.poll(async () => (await command("get_run", { session_id: session.session_id, run_id: accepted.run.run_id })).run.status).toBe("completed");
    sessions.push(session);
  }
  const [first, second] = sessions;

  // 只在浏览器端故障注入：保留正式 Host 的 SSE 字节与读取链路，按产品语义注入 gap 后关流。
  await page.addInitScript(() => {
    const faults = { injectGap: false, gaps: 0, streams: 0, otherSession: "", otherDeltas: 0 };
    Object.assign(window, { f02Faults: faults });
    const original = window.fetch.bind(window);
    window.fetch = async (...args) => {
      const response = await original(...args);
      const input = args[0];
      const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
      if (new URL(url, location.href).pathname !== "/events" || !response.body) return response;
      faults.streams++;
      const decoder = new TextDecoder();
      let pending = "";
      return new Response(response.body.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
        transform(chunk, output) {
          if (faults.injectGap) {
            faults.injectGap = false;
            faults.gaps++;
            output.enqueue(new TextEncoder().encode('event: stream_gap\ndata: {"dropped_events":1}\n\n'));
            output.terminate();
            return;
          }
          pending += decoder.decode(chunk, { stream: true });
          const frames = pending.split("\n\n");
          pending = frames.pop() ?? "";
          for (const frame of frames) {
            const data = frame.split("\n").find((line) => line.startsWith("data:"))?.slice(5).trim();
            if (!data || !frame.includes("event: runtime_event")) continue;
            const envelope = JSON.parse(data);
            if (envelope.event.type === "text_delta" && envelope.event.session_id === faults.otherSession) faults.otherDeltas++;
          }
          output.enqueue(chunk);
        },
      })), { status: response.status, headers: response.headers });
    };
  });
  type Faults = { injectGap: boolean; gaps: number; streams: number; otherSession: string; otherDeltas: number };
  let mode: "pass" | "busy" | "hold" = "pass";
  const counts = new Map<string, number>();
  const held: { release: () => void; done: Promise<void> }[] = [];
  let application_reads = 0;
  await page.route("**/commands", async (route) => {
    const request = route.request().postDataJSON();
    const command = request.command.payload;
    if (command.type === "get_application_snapshot") application_reads++;
    if (command.type !== "get_session_view") return route.continue();
    const id = command.payload.session_id as string;
    counts.set(id, (counts.get(id) ?? 0) + 1);
    if (id !== first.session_id || mode === "pass") return route.continue();
    if (mode === "busy") return route.fulfill({ status: 409, json: { request_id: request.request_id, error: { code: "snapshot_busy", message: "busy fixture" } } });
    const response = await route.fetch();
    const body = await response.json();
    body.result.payload.payload.snapshot.value.conversation.items = [];
    let release!: () => void;
    let completed!: () => void;
    const wait = new Promise<void>((resolve) => { release = resolve; });
    const done = new Promise<void>((resolve) => { completed = resolve; });
    held.push({ release, done });
    await wait;
    await route.fulfill({ response, json: body }).catch(() => {}); // 选择／超时取消后，迟到响应可以已被浏览器关闭。
    completed();
  });
  const issued = await fetch(`${host.base_url}/auth/login`, { method: "POST", headers, body: JSON.stringify({ method: "desktop" }) });
  expect(issued.ok).toBe(true);
  const { token } = await issued.json() as { token: string };
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto(`${host.base_url}/#token=${encodeURIComponent(token)}`);
  const navigation = page.getByRole("complementary", { name: "会话导航" });
  await navigation.getByRole("button", { name: second.title, exact: true }).click();
  await expect(page.getByText("离线回复：QUEUED_CASE", { exact: true })).toBeVisible();
  await page.clock.install();
  await page.clock.pauseAt(new Date(Date.now() + 1_000));

  mode = "busy";
  const before_busy = counts.get(first.session_id) ?? 0;
  await navigation.getByRole("button", { name: first.title, exact: true }).click();
  await expect.poll(() => counts.get(first.session_id)).toBe(before_busy + 1);
  await page.clock.runFor(250);
  await expect.poll(() => counts.get(first.session_id)).toBe(before_busy + 2);
  await page.clock.runFor(750);
  await expect(page.getByText("会话暂时繁忙，请重试", { exact: true })).toBeVisible();
  expect(counts.get(first.session_id)).toBe(before_busy + 3);
  mode = "pass";
  await page.getByRole("button", { name: "重新读取", exact: true }).click();
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();

  await navigation.getByRole("button", { name: second.title, exact: true }).click();
  mode = "hold";
  await navigation.getByRole("button", { name: first.title, exact: true }).click();
  await expect.poll(() => held.length).toBe(1);
  await page.clock.runFor(15_000);
  await expect(page.getByRole("alert").filter({ hasText: "读取会话超时，请重试" })).toBeVisible();
  mode = "pass";
  await page.getByRole("button", { name: "重试", exact: true }).click();
  await expect(page.getByRole("alert").filter({ hasText: "读取会话超时，请重试" })).toHaveCount(0);
  held[0].release();
  await held[0].done;
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();

  await navigation.getByRole("button", { name: second.title, exact: true }).click();
  mode = "hold";
  await navigation.getByRole("button", { name: first.title, exact: true }).click();
  await expect.poll(() => held.length).toBe(2);
  await navigation.getByRole("button", { name: second.title, exact: true }).click();
  await expect(page.getByText("离线回复：QUEUED_CASE", { exact: true })).toBeVisible();
  mode = "pass";
  await navigation.getByRole("button", { name: first.title, exact: true }).click();
  held[1].release();
  await held[1].done;
  await expect(page.getByText("离线回复：FIRST_CASE", { exact: true })).toBeVisible();
  await page.clock.resume();

  const stream_session = (await command("create_session", { title: "F02 并发输出", workspace_id: app.workspaces[0].workspace_id })).session;
  await page.evaluate((id) => { (window as unknown as { f02Faults: Faults }).f02Faults.otherSession = id; }, stream_session.session_id);
  const streaming = await command("submit_input", { session_id: stream_session.session_id, message: "F02_STREAM", mode: "normal", variant: "build", attachment_ids: [], quotes: [] });
  await expect.poll(() => page.evaluate(() => (window as unknown as { f02Faults: Faults }).f02Faults.otherDeltas)).toBeGreaterThan(5);
  const second_reads = counts.get(second.session_id) ?? 0;
  await navigation.getByRole("button", { name: second.title, exact: true }).click();
  await expect.poll(() => counts.get(second.session_id)).toBe(second_reads + 1);
  await expect(page.getByText("离线回复：QUEUED_CASE", { exact: true })).toBeVisible();
  expect((await command("get_run", { session_id: stream_session.session_id, run_id: streaming.run.run_id })).run.status).toBe("running");

  const before_gap = application_reads;
  await page.evaluate(() => { (window as unknown as { f02Faults: Faults }).f02Faults.injectGap = true; });
  await command("rename_session", { session_id: second.session_id, title: "F02 gap 已恢复" });
  await expect.poll(() => page.evaluate(() => (window as unknown as { f02Faults: Faults }).f02Faults.gaps)).toBe(1);
  await expect(navigation.getByRole("button", { name: "F02 gap 已恢复", exact: true })).toBeVisible();
  expect(application_reads).toBeGreaterThan(before_gap);
  await expect.poll(() => page.evaluate(() => (window as unknown as { f02Faults: Faults }).f02Faults.streams)).toBeGreaterThan(1);
  await expect.poll(async () => (await command("get_run", { session_id: stream_session.session_id, run_id: streaming.run.run_id })).run.status).toBe("completed");
  await expect(page.getByText("离线回复：QUEUED_CASE", { exact: true })).toHaveCount(1);
  await expect(page.getByText("正在读取会话", { exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});
