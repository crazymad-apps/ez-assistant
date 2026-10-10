import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { FeedbackCenterStore } from "../../src/features/feedback-center/FeedbackCenterStore";
import { FeedbackPanel } from "../../src/features/feedback-center/FeedbackPanel";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import type { SubmittedFeedback } from "@ez-assistant/protocol";

function fixture() {
  let scope = "alice";
  const command = vi.fn().mockResolvedValue({ payload: { plugins: [] } });
  let client = { command } as unknown as RuntimeClient;
  const store = new FeedbackCenterStore({ client: () => client, scope: () => scope });
  store.setScope(scope); store.create(); command.mockClear();
  store.edit({ kind: "platform", title: "预览失败", description: "打开文件时失败" });
  return { store, command, reconnect() { client = { command } as unknown as RuntimeClient; }, switchUser() { scope = "bob"; store.setScope(scope); } };
}
const failure = { payload: { status: "failure", data: "submission_unknown" } };
afterEach(cleanup);
describe("反馈中心草稿与回执", () => {
  it("未知结果先核对，未查到后同次重试复用原标识和内容", async () => {
    const { store, command } = fixture(); command.mockResolvedValue(failure);
    await store.submit(); const request = command.mock.calls[0][0].payload;
    expect(store.unknown).toBe(true);
    store.edit({ title: "不能修改" }); expect(store.draft.title).toBe("预览失败");
    await store.submit(); expect(command).toHaveBeenCalledTimes(1);
    command.mockResolvedValue({ payload: { status: "success", data: { items: [], total: 0, limit: 20, offset: 0 } } });
    await store.checkSubmission(); expect(command.mock.calls[1][0].payload.submission_id).toBe(request.submission_id);
    expect(store.checked).toBe(true); expect(store.message).toContain("暂未查到");
    command.mockResolvedValue(failure); await store.submit(); expect(command.mock.calls[2][0].payload).toEqual(request);
  });
  it("重复点击只发送一次，换账号丢弃迟到回执及原草稿", async () => {
    const { store, command, switchUser } = fixture();
    let resolve!: (value: unknown) => void;
    command.mockReturnValue(new Promise((done) => { resolve = done; }));
    const pending = store.submit(); await store.submit(); expect(command).toHaveBeenCalledTimes(1);
    switchUser(); resolve({ payload: { status: "success", data: { id: "old" } } }); await pending;
    expect(store.detail).toBeNull(); expect(store.open).toBe(false); expect(store.draft.title).toBe(""); expect(store.busy).toBe(false);
  });
  it("同一身份重连不接受旧客户端回执，也不会永远停在提交中", async () => {
    const { store, command, reconnect } = fixture(); let resolve!: (value: unknown) => void;
    command.mockReturnValue(new Promise((done) => { resolve = done; }));
    const pending = store.submit(); reconnect(); resolve({ payload: { status: "success", data: { id: "late" } } }); await pending;
    expect(store.detail).toBeNull(); expect(store.busy).toBe(false); expect(store.unknown).toBe(true); expect(store.frozen).not.toBeNull();
  });
  it("传输中断保留未知；查到后读取真实详情，不自动再次写入", async () => {
    const { store, command } = fixture(); command.mockRejectedValueOnce(new Error("lost receipt")); await store.submit();
    expect(store.unknown).toBe(true);
    const detail = { id: "saved", status: "resolved", handling_note: "已修复" } as SubmittedFeedback;
    command.mockResolvedValueOnce({ payload: { status: "success", data: { items: [{ id: "saved" }] } } });
    command.mockResolvedValueOnce({ payload: { status: "success", data: detail } });
    await store.checkSubmission(); expect(store.detail).toEqual(detail); expect(store.view).toBe("detail"); expect(store.frozen).toBeNull();
    expect(command.mock.calls.filter(([request]) => request.type === "submit_feedback")).toHaveLength(1);
  });
  it("明确拒绝允许编辑，改变内容生成新标识；超出字节限制不发送", async () => {
    const { store, command } = fixture(); command.mockResolvedValue({ payload: { status: "failure", data: "invalid_input" } });
    await store.submit(); const first = store.frozen!.submission_id;
    store.edit({ title: "补全信息" }); await store.submit(); expect(store.frozen!.submission_id).not.toBe(first);
    command.mockClear(); store.edit({ description: "中".repeat(4000), reproduction: "中".repeat(1000) }); await store.submit();
    expect(command).not.toHaveBeenCalled(); expect(store.message).toContain("内容过长");
  });
  it("草稿关闭需要确认，保留或放弃均不发请求", () => {
    const { store, command } = fixture(); store.open = true;
    render(<FeedbackPanel store={store} />); fireEvent.click(screen.getByRole("button", { name: "关闭反馈中心" }));
    expect(screen.getByRole("dialog", { name: "放弃本地草稿" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续编辑" })); expect(store.draft.title).toBe("预览失败");
    fireEvent.click(screen.getByRole("button", { name: "关闭反馈中心" })); fireEvent.click(screen.getByRole("button", { name: "放弃草稿" }));
    expect(store.open).toBe(false); expect(store.draft.title).toBe(""); expect(command).not.toHaveBeenCalled();
  });
});
