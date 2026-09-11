import { spawn } from "node:child_process";
/** 只传普通登录 URL；无图形环境时由用户手动打开，不传递任何凭据。 */
export async function openBrowser(address: string): Promise<boolean> {
  if (process.env.SSH_CONNECTION || (process.platform === "linux" && !process.env.DISPLAY && !process.env.WAYLAND_DISPLAY)) return false;
  const command = process.platform === "darwin" ? "open" : "xdg-open";
  if (!["darwin", "linux"].includes(process.platform)) return false;
  return new Promise((resolve) => {
    const child = spawn(command, [address], { stdio: "ignore" });
    let finished = false;
    const finish = (result: boolean) => { if (!finished) { finished = true; clearTimeout(timer); resolve(result); } };
    const timer = setTimeout(() => { child.unref(); finish(false); }, 3000);
    child.on("error", () => finish(false)); child.on("close", (code) => finish(code === 0));
  });
}
