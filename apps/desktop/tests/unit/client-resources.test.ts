import { afterEach, expect, it, vi } from "vitest";
import { fireEvent } from "@testing-library/react";
import { ClientResources } from "../../src/runtime-client/ClientResources";
import { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { hostFilePath } from "../../src/runtime-client/hostFilePath";
import {
  loadDesktopPreferences,
  saveDesktopPreferences,
  viewingSnapshot,
} from "../../src/native-bridge/desktopPreferences";
import { copyText } from "../../src/platform/clipboard";
import type { SessionMaterializationManifest } from "../../src/generated/assistant-protocol";

vi.mock("@tauri-apps/api/core", async (original) => ({
  ...(await original<typeof import("@tauri-apps/api/core")>()),
  isTauri: () => false,
}));
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
  document.body.replaceChildren();
});
function runtime(origin = "http://host.test:7240") {
  return new RuntimeClient({
    base_url: origin,
    access_token: "memory-only",
    instance_id: "host-a",
    started_runtime: false,
    capabilities: {
      protocol_version: 1,
      runtime_version: "test",
      max_command_bytes: 65536,
      max_attachment_bytes: null,
      sse: true,
      streaming_upload: true,
      features: ["session_materialization"],
    },
  });
}
async function choose(resources: ClientResources, file: File) {
  vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(
    () => undefined,
  );
  const pending = resources.chooseAttachmentFiles();
  fireEvent.change(document.querySelector('input[type="file"]')!, {
    target: { files: [file] },
  });
  return (await pending)[0]!;
}

it("uploads the browser File to its chosen Host and reuses the materialization key after an uncertain response", async () => {
  const client = runtime();
  const resources = new ClientResources(() => client, false);
  const file = new File(["CLIENT BYTES"], "same.txt", { type: "text/plain" });
  const selected = await choose(resources, file);
  const requests: RequestInit[] = [];
  const fetcher = vi
    .fn()
    .mockImplementationOnce(async (_url, init: RequestInit) => {
      requests.push(init);
      throw new TypeError("network closed");
    })
    .mockImplementationOnce(async (_url, init: RequestInit) => {
      requests.push(init);
      return Response.json({
        session: { session_id: "committed" },
        attachments: [],
      });
    });
  vi.stubGlobal("fetch", fetcher);
  const manifest: SessionMaterializationManifest = {
    idempotency_key: "stable-attempt",
    variant: "build",
    approval_mode: "auto",
    mode: "normal",
    message: "send",
    attachments: [
      {
        selection_key: selected.selection_id,
        original_name: file.name,
        size_bytes: file.size,
      },
    ],
  };
  await expect(
    resources.materializeNewSession(manifest, "operation-a"),
  ).rejects.toMatchObject({ code: "materialization_response_unknown" });
  await expect(
    resources.materializeNewSession(manifest, "operation-b"),
  ).resolves.toMatchObject({ session: { session_id: "committed" } });
  for (const init of requests) {
    const form = init.body as FormData;
    expect([...form.keys()][0]).toBe("manifest");
    expect(JSON.parse(String(form.get("manifest"))).idempotency_key).toBe(
      "stable-attempt",
    );
    expect((form.get(selected.selection_id) as File).name).toBe("same.txt");
    expect(new Headers(init.headers).get("Authorization")).toBe(
      "Bearer memory-only",
    );
  }
  expect(
    fetcher.mock.calls.every(
      ([url]) => url === "http://host.test:7240/session-materializations",
    ),
  ).toBe(true);
  await expect(
    resources.previewAttachmentSelection(selected.selection_id),
  ).rejects.toThrow("失效");
  resources.dispose();
  client.dispose();
});

it("cancels pending upload and rejects late results when a connection is disposed", async () => {
  const client = runtime();
  let current: RuntimeClient | null = client;
  const resources = new ClientResources(() => current, false);
  const selected = await choose(resources, new File(["a"], "file.txt"));
  let signal: AbortSignal | undefined;
  let finish!: (r: Response) => void;
  vi.stubGlobal(
    "fetch",
    vi.fn((_url, init: RequestInit) => {
      signal = init.signal as AbortSignal;
      return new Promise<Response>((r) => {
        finish = r;
      });
    }),
  );
  const result = resources.uploadSelectedAttachment(
    "session",
    selected.selection_id,
    "upload",
  );
  resources.dispose();
  client.dispose();
  current = null;
  expect(signal?.aborted).toBe(true);
  finish(Response.json({ attachment: { attachment_id: "old-host" } }));
  await expect(result).rejects.toThrow("连接已变化");
  await expect(
    resources.previewAttachmentSelection(selected.selection_id),
  ).rejects.toThrow("失效");
});

it("closes a pending browser chooser on disposal and resolves Host references without native registration", async () => {
  const resources = new ClientResources(() => null, false);
  vi.spyOn(HTMLInputElement.prototype, "click").mockImplementation(
    () => undefined,
  );
  const choice = resources.chooseAttachmentFiles();
  resources.dispose();
  await expect(choice).resolves.toEqual([]);
  expect(document.querySelector('input[type="file"]')).toBeNull();
  expect(hostFilePath("../图.png", "/workspace/docs/readme.md")).toBe(
    "/workspace/图.png",
  );
  expect(hostFilePath("file:///host/a%20b.txt")).toBe("/host/a b.txt");
  expect(() => hostFilePath("file://another-host/file")).toThrow();
});

it("persists Web view descriptions without terminal, browser, local-file handles or file contents", async () => {
  const snapshot = {
    current_scope_key: "session:a",
    groups: [
      {
        scope_key: "session:a",
        active_index: 3,
        focused_index: 3,
        tabs: [
          { page: { type: "context" as const } },
          {
            page: {
              type: "terminal" as const,
              source: { type: "workspace" as const, workspace_id: "a" },
            },
          },
          {
            page: {
              type: "resource" as const,
              name: "client.txt",
              source: {
                type: "local_file" as const,
                path_segments: ["/", "client.txt"],
              },
              line: null,
            },
          },
          {
            page: {
              type: "resource" as const,
              name: "host.txt",
              source: {
                type: "host_file" as const,
                path: "/workspace/host.txt",
              },
              line: null,
            },
          },
        ],
      },
    ],
  };
  const preferences = await loadDesktopPreferences();
  await saveDesktopPreferences({
    ...preferences,
    resource_workspace: snapshot,
  });
  const saved = await loadDesktopPreferences();
  expect(saved.resource_workspace?.groups[0].tabs).toHaveLength(2);
  expect(saved.resource_workspace?.groups[0].active_index).toBe(1);
  expect(viewingSnapshot(snapshot, true)?.groups[0].tabs).toHaveLength(4);
  expect(localStorage.getItem("ez-assistant:view")).not.toContain("client.txt");
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new DOMException("quota", "QuotaExceededError");
  });
  await expect(saveDesktopPreferences(preferences)).rejects.toThrow("quota");
});

it("copies on ordinary HTTP without Clipboard API and cleans the temporary selection", async () => {
  const copy = vi.fn(() => true);
  Object.defineProperty(document, "execCommand", {
    value: copy,
    configurable: true,
  });
  await copyText("/host/path");
  expect(copy).toHaveBeenCalledWith("copy");
  expect(document.querySelector("textarea")).toBeNull();
  Object.defineProperty(document, "execCommand", {
    value: () => false,
    configurable: true,
  });
  await expect(copyText("/host/path")).rejects.toThrow(
    "手动选择复制：/host/path",
  );
});
