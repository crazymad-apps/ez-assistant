import { afterEach, expect, it, vi } from "vitest";
import { DesktopInputChannel } from "../../src/runtime-client/DesktopInputChannel";
import { desktopPoint } from "../../src/features/resource-workspace/ResourceDesktop/inputGeometry";

const cleanup: (() => void)[] = [];
afterEach(() => { cleanup.splice(0).forEach((f) => f()); vi.useRealTimers(); });
function fixture() {
  vi.useFakeTimers();
  const command = vi.fn(), receive = vi.fn();
  const channel = { readyState: "open", bufferedAmount: 0, send: vi.fn(), onmessage: null } as unknown as RTCDataChannel;
  const input = new DesktopInputChannel(channel, command, receive);
  cleanup.push(() => input.close());
  const message = (value: object) => channel.onmessage?.({ data: JSON.stringify(value) } as MessageEvent);
  const ready = (id = "control", request_id = 1) => message({ type: "control_ready", request_id, view_id: "view", control_id: id, display_id: "screen" });
  const ack = (sequence: number, control_id = "control") => message({ type: "input_ack", control_id, sequence });
  return { command, receive, channel, input, message, ready, ack };
}
it("留白不产生输入，实际画面和原始尺寸都使用同一归一化坐标", () => {
  const rect = { left: 10, top: 20, width: 1000, height: 1000 };
  expect(desktopPoint(rect, 1000, 500, 510, 520)).toEqual({ x: .5, y: .5 });
  expect(desktopPoint(rect, 1000, 500, 510, 21)).toBeNull();
  expect(desktopPoint(rect, 0, 500, 510, 520)).toBeNull();
});
it("未获确认不发送输入，旧申请响应不能重新开启控制", async () => {
  const f = fixture();
  await expect(f.input.send({ type: "heartbeat" })).rejects.toThrow();
  f.input.start(); f.input.stop(); f.input.start();
  f.ready("old", 1);
  expect(f.channel.send).not.toHaveBeenCalled();
  f.ready("new", 2);
  expect(f.channel.send).toHaveBeenCalledTimes(1);
  f.ack(1, "new");
  f.ready("new", 2);
  expect(f.channel.send).toHaveBeenCalledTimes(1);
});
it("超过 250ms 未确认通过独立控制面停止，不重放输入", async () => {
  const f = fixture(); f.input.start(); f.ready(); f.ack(1);
  const sending = f.input.send({ type: "key", code: "KeyA", down: true, repeat: false });
  const failed = expect(sending).rejects.toThrow("未确认");
  await vi.advanceTimersByTimeAsync(300);
  await failed;
  expect(f.command).toHaveBeenLastCalledWith({ type: "control_stop" });
  expect(f.receive).toHaveBeenLastCalledWith(expect.objectContaining({ type: "control_stopped", message: expect.stringContaining("超时") }));
});
it("文本先完整限长，顺序传输并等待确认，不拆 emoji", async () => {
  const f = fixture(); f.input.start(); f.ready(); f.ack(1);
  await expect(f.input.text("a".repeat(16385))).rejects.toThrow();
  const frames: Array<{ sequence: number; input: { type: string; text?: string } }> = [];
  vi.mocked(f.channel.send).mockImplementation((raw) => { const frame = JSON.parse(String(raw)); frames.push(frame); queueMicrotask(() => f.ack(frame.sequence)); });
  const text = "中文🙂".repeat(300);
  await f.input.text(text);
  expect(frames[0]!.input.type).toBe("release");
  expect(frames.at(-1)!.input.type).toBe("text_end");
  expect(frames.filter((f) => f.input.type === "text_chunk").map((f) => f.input.text).join("")).toBe(text);
  expect(frames.every((f) => new TextEncoder().encode(JSON.stringify(f)).length <= 4096)).toBe(true);
});
