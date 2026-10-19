import { DesktopInputChannel, type DesktopControlEvent } from "./DesktopInputChannel";
import { desktopVideoAnswer } from "./desktopVideoAnswer";
import { currentCompatibility, type DesktopDisplay, type DesktopInput, type DesktopViewOpenRequest } from "@ez-assistant/protocol";

export type DesktopEvent = { type: "displays"; displays: readonly DesktopDisplay[]; selected: string | null } | DesktopControlEvent | { type: "stream"; stream: MediaStream } | { type: "ready" } | { type: "error"; message: string };
const errors: Record<string, string> = {
  desktop_display_unavailable: "显示器已移除或布局发生变化，请重新连接后选择显示器。",
  desktop_busy: "目标设备的桌面查看已达到并发上限，请稍后重试。",
  desktop_slow_viewer: "当前连接无法及时接收桌面画面，请检查网络后重新连接。",
  desktop_access_denied: "当前账号没有桌面查看权限，或登录已失效。",
  desktop_direct_unavailable: "无法建立桌面直连，请检查两台设备的网络后重试。",
  desktop_capture_unavailable: "目标设备无法采集画面，请检查屏幕录制权限和当前桌面。",
  desktop_connection_ended: "桌面连接已结束，请重新连接。",
};

/** UI 只与当前本机 Host 协商播放；没有远端地址、中心凭据或穿透策略。 */
export class DesktopSocket {
  readonly #socket: WebSocket;
  readonly #input: DesktopInputChannel;
  readonly #peer: RTCPeerConnection;
  #closed = false;
  #offered = false;
  #decoded = false;
  #queued = 0;
  #serial = Promise.resolve();
  #timer: ReturnType<typeof setTimeout>;
  readonly #cancel: () => void;

  constructor(address: string, bearer: string, device_id: string, login_context: string | null,
    private readonly receive: (event: DesktopEvent) => void, private readonly signal: AbortSignal, display_id: string | null = null) {
    const url = new URL("/desktop-view/socket", address);
    if (!["127.0.0.1", "localhost", "[::1]"].includes(url.hostname)) throw new Error("请通过本机 Runtime 打开远程桌面。");
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    this.#peer = new RTCPeerConnection({ iceServers: [] });
    this.#socket = new WebSocket(url);
    this.#input = new DesktopInputChannel(this.#peer.createDataChannel("desktop-input", { negotiated: true, id: 0, ordered: true }), (value) => this.#send(value), receive);
    this.#cancel = () => this.#fail("桌面连接已结束，请重新连接。");
    signal.addEventListener("abort", this.#cancel, { once: true });
    this.#timer = setTimeout(() => this.#fail("桌面连接超时，请重新连接。"), 60_000);
    this.#socket.onopen = () => {
      if (this.#closed) { this.disconnect(); return; }
      const data: DesktopViewOpenRequest = { device_id, display_id, bearer: bearer || null, login_context, client_compatibility: currentCompatibility() };
      this.#send({ type: "open", data });
    };
    this.#socket.onmessage = ({ data }: MessageEvent<unknown>) => {
      if (this.#closed) return;
      if (typeof data !== "string" || data.length > 16_384 || ++this.#queued > 72) { this.#fail("桌面协商消息无效。"); return; }
      this.#serial = this.#serial.then(async () => {
        if (!this.#closed) await this.#message(JSON.parse(data) as unknown);
      }).catch(() => this.#fail("无法建立本机桌面播放连接。")).finally(() => { this.#queued -= 1; });
    };
    this.#socket.onerror = () => this.#fail("无法连接本机桌面服务，请确认 Host 版本。");
    this.#socket.onclose = () => { if (!this.#closed) this.#fail("桌面连接已结束，请重新连接。"); };
    this.#peer.onicecandidate = ({ candidate }) => {
      if (this.#closed || !candidate) return;
      // Host 主动向 WebView 发起 loopback 检查；不向 Host 提交其他网卡候选。
      const address = candidate.candidate.split(/\s+/)[4];
      if (address === "127.0.0.1" || address === "::1") this.#send({ type: "candidate", data: candidate.toJSON() });
    };
    this.#peer.ontrack = ({ track, streams }) => {
      if (this.#closed) { track.stop(); return; }
      this.receive({ type: "stream", stream: streams[0] ?? new MediaStream([track]) });
      track.onended = () => this.#fail("目标桌面画面已停止。");
    };
    this.#peer.onconnectionstatechange = () => {
      if (["failed", "disconnected", "closed"].includes(this.#peer.connectionState) && !this.#closed) this.#fail("桌面连接已断开，请重新连接。");
    };
    if (signal.aborted) this.#cancel();
  }

  async #message(value: unknown): Promise<void> {
    if (!value || typeof value !== "object" || !("type" in value)) throw new Error();
    const message = value as { type: unknown; data?: unknown; code?: unknown };
    if (message.type === "offer" && !this.#offered) {
      this.#offered = true;
      const offer = message.data as RTCSessionDescriptionInit;
      if (offer?.type !== "offer" || typeof offer.sdp !== "string" || offer.sdp.length > 8192) throw new Error();
      await this.#peer.setRemoteDescription(offer);
      if (this.#closed) return;
      const answer = await desktopVideoAnswer(await this.#peer.createAnswer());
      if (this.#closed) return;
      if (!answer) { this.#fail("当前浏览器不支持此高清桌面的解码规格，请更新客户端或浏览器后重试。"); return; }
      await this.#peer.setLocalDescription(answer);
      if (!this.#closed) this.#send({ type: "answer", data: answer });
    } else if (message.type === "candidate" && this.#offered) {
      const candidate = message.data as RTCIceCandidateInit;
      if (typeof candidate?.candidate !== "string" || candidate.candidate.length > 1024) throw new Error();
      const address = candidate.candidate.split(/\s+/)[4];
      if (address !== "127.0.0.1" && address !== "::1") throw new Error();
      await this.#peer.addIceCandidate(candidate);
    } else if (message.type === "displays") {
      const data = message.data as { displays?: DesktopDisplay[]; selected?: string | null };
      if (!Array.isArray(data?.displays) || !data.displays.length || data.displays.length > 16
        || data.displays.some((d) => !d || typeof d.id !== "string" || !d.id || d.id.length > 256 || [...d.id].some((char) => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)
          || !Number.isInteger(d.width) || d.width <= 0 || d.width > 16384 || !Number.isInteger(d.height) || d.height <= 0 || d.height > 16384 || typeof d.primary !== "boolean")
        || new Set(data.displays.map((d) => d.id)).size !== data.displays.length || (data.selected !== null && !data.displays.some((d) => d.id === data.selected))) throw new Error();
      this.receive({ type: "displays", displays: data.displays, selected: data.selected! });
    } else if (message.type === "ready" && this.#decoded) {
      clearTimeout(this.#timer);
      this.receive({ type: "ready" });
    } else if (message.type === "error") {
      this.#fail(typeof message.code === "string" ? errors[message.code] ?? errors.desktop_connection_ended! : errors.desktop_connection_ended!);
    } else throw new Error();
  }

  startControl(): void { if (this.#decoded && !this.#closed) this.#input.start(); }

  stopControl(): void { this.#input.stop(); }

  input(value: DesktopInput): Promise<void> { return this.#input.send(value); }

  sendText(value: string): Promise<void> { return this.#input.text(value); }

  decoded(): void {
    if (!this.#closed && !this.#decoded) { this.#decoded = true; this.#send({ type: "decoded" }); }
  }

  #send(value: unknown): void {
    if (this.#closed || this.#socket.readyState !== WebSocket.OPEN) return;
    if (this.#socket.bufferedAmount > 65_536) { this.#fail("桌面协商连接拥塞，请重试。"); return; }
    this.#socket.send(JSON.stringify(value));
  }

  #fail(message: string): void {
    if (this.#closed) return;
    this.disconnect();
    this.receive({ type: "error", message });
  }

  disconnect(): void {
    if (this.#closed) return;
    this.#input.close();
    this.#send({ type: "close" });
    this.#closed = true;
    clearTimeout(this.#timer);
    this.signal.removeEventListener("abort", this.#cancel);
    this.#peer.close();
    this.#socket.close();
  }
}
