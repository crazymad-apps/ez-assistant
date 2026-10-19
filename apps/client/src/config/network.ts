import * as p from "@clack/prompts";
import { hostname } from "node:os";
import type { HostRegistrationCommand } from "@ez-assistant/protocol/node";
import { ClientError, Cancelled, object } from "../errors.js";
import type { HostControl } from "../host/control.js";
import { request } from "../host/http.js";
import { sameInstance } from "../host/discovery.js";
import { enterpriseLogin, registration, registrationLines } from "../host/registration.js";
import { note, safe } from "../terminal/output.js";

function answer<T>(value: T | symbol): T {
  if (p.isCancel(value)) throw new Cancelled();
  return value as T;
}

/** 只管理首次核验的本机实例；密码和普通登录凭据仅在本次交互内存中存在。 */
export async function configureNetwork(host: HostControl): Promise<boolean> {
  const target = await host.status();
  if (!target) {
    p.log.warn("Host 未启动，请先通过 start 启动后再申请登记。");
    return false;
  }
  if (target.capabilities.mode !== "enterprise") {
    p.log.info("请先在 Host 访问设置中配置企业模式并重启。");
    return false;
  }
  // 与桌面管理共用当前进程来源查询，不把待重启的已保存地址当作登记中心。
  const context = object(await request(target.discovery, "/desktop-update-context", undefined, host.signal));
  if (typeof context.center_url !== "string") throw new ClientError("invalid_response", "当前企业中心地址无法核实。");
  note(context.center_url, "当前企业中心");
  let token: string | undefined;
  let failed = false;
  let blocked = false;
  let status = await registration(target.discovery, { type: "get_status" }, host.signal);
  for (;;) {
    note(registrationLines(status).join("\n"), "企业组网 · 本机登记");
    const endPending = status.intent === "exit" || status.intent === "withdraw";
    const options: { value: string; label: string }[] = [{ value: "query", label: "查询最新状态" }];
    if (!blocked && !status.error && !endPending) {
      if (!status.device || ["rejected", "withdrawn", "revoked"].includes(status.device.status))
        options.push({ value: "apply", label: status.registration_id ? "重新申请" : "申请登记" });
      if (status.device?.status === "pending") options.push({ value: "withdraw", label: "撤回申请" });
      if (status.device?.status === "registered") options.push({ value: "exit", label: "退出组网" });
    }
    options.push({ value: "back", label: "返回" });
    const action = answer(await p.select({ message: "企业组网", options, signal: host.signal }));
    if (action === "back") return failed;
    try {
      const current = await host.target();
      if (!current || !sameInstance(current.discovery, target.discovery))
        throw new ClientError("instance_changed", "Host 实例已变化，请返回后重新进入。");
      let command: HostRegistrationCommand = { type: "get_status" };
      if (action === "apply") {
        if (!token) {
          const username = answer(
            await p.text({
              message: "登录企业账号",
              placeholder: "账号",
              signal: host.signal,
              validate: (value) => (!value?.trim() ? "请输入账号" : undefined),
            }),
          );
          const password = answer(
            await p.password({
              message: "企业密码",
              mask: "●",
              signal: host.signal,
              validate: (value) => (!value ? "请输入密码" : undefined),
            }),
          );
          const login = await enterpriseLogin(target.discovery, username.trim(), password, host.signal);
          token = login.token ?? undefined;
          p.log.success("已登录企业账号；关闭 Client 不退出 Host 用户。");
        }
        const name = answer(
          await p.text({
            message: "设备名称",
            initialValue: hostname(),
            placeholder: "设备名称",
            signal: host.signal,
            validate: (value) =>
              !value?.trim() ||
              value.trim().length > 80 ||
              [...value].some(
                (char) => char.charCodeAt(0) < 32 || (char.charCodeAt(0) >= 127 && char.charCodeAt(0) <= 159),
              )
                ? "请输入 1–80 字的设备名称，不含控制字符。"
                : undefined,
          }),
        );
        command = { type: "apply", payload: { name: name.trim() } };
      } else if (action === "withdraw" || action === "exit") {
        const message =
          action === "withdraw"
            ? "撤回这台电脑的登记申请？"
            : "退出企业组网？已有组网访问将停止，本机文件、会话和任务保留，原有直连登录不变。";
        if (!answer(await p.confirm({ message, initialValue: false, signal: host.signal }))) continue;
        command = { type: action };
      }
      // 写请求绝不自动重放；未知结果只能先查询原登记。
      if (command.type !== "get_status") blocked = true;
      status = await registration(target.discovery, command, host.signal, token);
      blocked = !!status.error;
      failed = !!status.error;
    } catch (error) {
      if (!(error instanceof ClientError)) throw error;
      failed = true;
      if (error.code === "authentication_required" || error.code === "unauthorized") token = undefined;
      p.log.error(safe(`${error.message} 请查询最新状态后再操作。`));
    }
  }
}
