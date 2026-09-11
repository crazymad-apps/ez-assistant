import { createConnection } from "node:net";
import { ClientError, Cancelled } from "../errors.js";
/** 只连接本机配置端口，不发送认证或应用数据；最终绑定仍由 Host 裁决。 */
export function portOccupied(port: number, signal: AbortSignal): Promise<boolean> {
  return new Promise((resolve, reject) => {
    const socket = createConnection({ host: "127.0.0.1", port }); let done = false;
    const finish = (error?: Error, occupied = false) => {
      if (done) return; done = true; clearTimeout(timer); signal.removeEventListener("abort", cancel); socket.destroy();
      if (error) reject(error); else resolve(occupied);
    };
    const timer = setTimeout(() => finish(new ClientError("port_unknown", "无法核实本机端口状态，请检查网络与监听配置。")), 1000);
    const cancel = () => finish(new Cancelled());
    signal.addEventListener("abort", cancel, { once: true });
    socket.on("connect", () => finish(undefined, true));
    socket.on("error", (error: NodeJS.ErrnoException) => error.code === "ECONNREFUSED" ? finish() : finish(new ClientError("port_unknown", "无法核实本机端口状态。")));
    if (signal.aborted) cancel();
  });
}
