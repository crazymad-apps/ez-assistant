import { execFile } from "node:child_process";
import { ClientError } from "../../errors.js";

/** 有界系统命令；中断命令不代表 OS 已撤销操作，调用方必须回读。 */
export async function serviceCommand(program: "systemctl" | "loginctl" | "flock", args: string[]): Promise<void> {
  await new Promise<void>((accept, reject) => {
    execFile(program, args, { timeout: 10000, maxBuffer: 65536, env: { ...process.env, LC_ALL: "C", SYSTEMD_PAGER: "", SYSTEMD_COLORS: "0" } }, (error) => {
      if (error) reject(new ClientError("service_command_failed", `${program} ${args.join(" ")} 未确认成功；可能是权限拒绝或超时，请回读实际状态。`));
      else accept();
    });
  });
}
