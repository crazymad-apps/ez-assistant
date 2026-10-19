import { afterEach, expect, it, vi } from "vitest";
import { desktopVideoAnswer } from "../../src/runtime-client/desktopVideoAnswer";

afterEach(() => vi.unstubAllGlobals());
const answer = { type: "answer" as const, sdp: "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\na=recvonly\r\na=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n" };

it("确认横竖高清解码能力后才提高接收档位，保留输入通道", async () => {
  const decodingInfo = vi.fn().mockResolvedValue({ supported: true });
  vi.stubGlobal("navigator", { mediaCapabilities: { decodingInfo } });
  const result = await desktopVideoAnswer(answer);
  expect(result?.sdp).toContain("profile-level-id=42e032");
  expect(result?.sdp).toContain("m=application 9 UDP/DTLS/SCTP webrtc-datachannel");
  expect(decodingInfo).toHaveBeenCalledWith(expect.objectContaining({ type: "webrtc", video: expect.objectContaining({ width: 1920, height: 1440 }) }));
  expect(decodingInfo).toHaveBeenCalledWith(expect.objectContaining({ video: expect.objectContaining({ width: 1440, height: 1920 }) }));
});

it("不支持或查询失败时不宣称高清兼容，也不改写非接收方向", async () => {
  const decodingInfo = vi.fn().mockResolvedValue({ supported: false });
  vi.stubGlobal("navigator", { mediaCapabilities: { decodingInfo } });
  expect(await desktopVideoAnswer(answer)).toBeNull();
  decodingInfo.mockRejectedValue(new Error("unsupported API"));
  expect(await desktopVideoAnswer(answer)).toBeNull();
  decodingInfo.mockClear();
  expect(await desktopVideoAnswer({ ...answer, sdp: answer.sdp.replace("a=recvonly", "a=sendrecv") })).toBeNull();
  expect(decodingInfo).not.toHaveBeenCalled();
});
