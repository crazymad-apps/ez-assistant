import * as p from "@clack/prompts";
import { ClientError, Cancelled, cancelled } from "../errors.js";
import { note, safe } from "../terminal/output.js";
import { querySystemd, serviceLines } from "../platform/systemd/query.js";
import { prepareService, saveService, servicePreview } from "../platform/systemd/commit.js";

/** 独立提交启动设置；不读取 Host 业务配置或修改访问草稿。 */
export async function configureStartup(home: string, signal: AbortSignal, saved: () => void): Promise<boolean> {
  let failed = false;
  const answer = <T>(value: T | symbol): T => { if (p.isCancel(value)) throw new Cancelled(); return value as T; };
  while (true) {
    cancelled(signal);
    const status = await querySystemd(home);
    note(serviceLines(status).join("\n"), "当前启动设置");
    if (status.kind !== "known") return status.kind !== "unsupported";
    const action = answer(await p.select({ message: "启动设置 · 独立保存", signal, showInstructions: false, options: [
      { value: "enable", label: "开启开机自启" }, { value: "disable", label: "关闭开机自启" },
      { value: "source", label: "切换为当前 Client 随包 Host", hint: "需先停止活动服务" },
      { value: "reload", label: "重新读取" }, { value: "back", label: "返回" },
    ] }));
    if (action === "back") return failed;
    if (action === "reload") continue;
    try {
      const enabled = action === "enable" || (action === "source" && status.state.fileState === "enabled");
      const draft = await prepareService(home, enabled, action === "source");
      note(servicePreview(draft).join("\n"), "启动设置预览 · 尚未保存");
      if (!answer(await p.confirm({ message: "保存这些启动设置？", initialValue: false, active: "保存", inactive: "取消", signal }))) continue;
      await saveService(draft, signal); saved(); failed = false;
      p.log.success("启动设置已保存；当前 Host 的运行状态不变。");
    } catch (error) {
      if (!(error instanceof ClientError)) throw error;
      failed = true; p.log.error(safe(error.message));
    }
  }
}
