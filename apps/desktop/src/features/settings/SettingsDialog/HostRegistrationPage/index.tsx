import { useEffect, useMemo, useState } from "react";
import type { HostNetworkResult } from "@ez-assistant/protocol";
import { observer } from "mobx-react-lite";
import { isTauri } from "@tauri-apps/api/core";
import { CollapsibleSection } from "../../../../components/CollapsibleSection";
import { TextInput } from "../../../../components/TextInput";
import { Button } from "../../../../components/Button";
import { Dialog } from "../../../../components/Dialog";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { manageLocalHost } from "../../../../native-bridge/runtimeConnection";
import { useApplicationConnection } from "../../../runtime-access/ApplicationConnectionContext";
import { SettingsPageContainer } from "../SettingsPageContainer";
import { HostRegistrationStore } from "./store";
import styles from "./index.module.scss";

export const HostRegistrationPage = observer(function HostRegistrationPage(props: {
  onBack: () => void;
  onDirtyChange: (dirty: boolean) => void;
  onPendingChange: (pending: boolean) => void;
}) {
  const root = useRootStore();
  const application = useApplicationConnection();
  const available = isTauri() && root.local_files.native_host && root.desktop_lifecycle.local_impact_known;
  const enterprise = root.local_connection.capabilities?.mode === "enterprise";
  const client = root.local_runtime_client;
  const store = useMemo(() => new HostRegistrationStore(), [client]);
  const [center, setCenter] = useState("");
  const [grant, setGrant] = useState<"loading" | "allowed" | "missing" | "unknown">("loading");
  const [confirm, setConfirm] = useState<"withdraw" | "exit" | null>(null);
  useEffect(() => {
    let active = true;
    if (available && enterprise) {
      store.start();
      void manageLocalHost({ type: "get_status" })
        .then((value) => {
          if (active) setCenter(value.current_center_url ?? value.center_url ?? "未配置");
        })
        .catch(() => {
          if (active) setCenter("地址暂时无法读取");
        });
    }
    return () => {
      active = false;
      store.dispose();
    };
  }, [available, enterprise, store]);
  useEffect(() => {
    props.onPendingChange(store.pending);
    return () => props.onPendingChange(false);
  }, [store.pending, props.onPendingChange]);
  const status = store.status;
  const state = status?.device?.status;
  useEffect(() => {
    if (!available || state !== "registered" || !status?.registration_id || !client) return;
    const controller = new AbortController();
    setGrant("loading");
    const deviceId = status.registration_id;
    void (async () => {
      let offset = 0;
      while (!controller.signal.aborted) {
        const result = await client.resource(
          "/host-network/access",
          {
            method: "POST",
            signal: controller.signal,
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ type: "list_devices", payload: { offset } }),
          },
          (response) => response.json() as Promise<HostNetworkResult>,
        );
        if (controller.signal.aborted) return;
        if (result.type !== "devices") throw new Error("设备目录响应不匹配");
        if (result.payload.items.some((device) => device.id === deviceId)) {
          setGrant("allowed");
          return;
        }
        if (result.payload.next_offset === null) {
          setGrant("missing");
          return;
        }
        if (result.payload.next_offset <= offset) throw new Error("设备目录分页异常");
        offset = result.payload.next_offset;
      }
    })().catch(() => {
      if (!controller.signal.aborted) setGrant("unknown");
    });
    return () => controller.abort();
  }, [available, state, status, client]);
  const canApply =
    !!status &&
    (!state || ["rejected", "withdrawn", "revoked"].includes(state)) &&
    !["exit", "withdraw"].includes(status.intent ?? "");
  useEffect(() => {
    props.onDirtyChange(store.pending || (canApply && store.name !== store.initialName));
    return () => props.onDirtyChange(false);
  }, [store.pending, store.name, store.initialName, canApply, props.onDirtyChange]);
  const labels = {
    pending: "等待管理员确认",
    registered: "已登记",
    rejected: "申请已拒绝",
    withdrawn: "申请已撤回",
    revoked: "已注销",
  };
  const connections = {
    disconnected: "未连接",
    connecting: "连接中",
    online: "已连接企业中心",
    retrying: "正在重连",
    conflict: "设备身份冲突，请核对原登记",
    incompatible: "协议不兼容，请更新中心或 Host",
    credential_invalid: "设备身份失效，请核对中心登记",
    failed: "连接失败",
  };
  return (
    <SettingsPageContainer
      title="企业组网"
      on_back={props.onBack}
      back_label="返回 Runtime"
      actions={
        available &&
        enterprise && (
          <Button variant="text" disabled={store.pending || store.loading} onClick={() => void store.refresh()}>
            刷新状态
          </Button>
        )
      }
    >
      {!available ? (
        <p>请连接本机服务后管理这台电脑的登记。</p>
      ) : !enterprise ? (
        <p>请先在设置 → Runtime → 访问设置中配置企业中心。</p>
      ) : (
        <div className={styles.content}>
          <section className={styles.section}>
            <h4>企业中心</h4>
            <dl className={styles.rows}>
              <div><dt>中心地址</dt><dd>{center || "正在读取…"}</dd></div>
              <div><dt>当前账号</dt><dd>{application?.account_label ?? "企业用户"}</dd></div>
            </dl>
          </section>
          <section className={styles.section}>
            <h4>本机登记</h4>
            {store.error && (
              <p role="alert" className={styles.error}>
                状态未更新：{store.error}
              </p>
            )}
            {!status ? (
              <p>{store.loading ? "正在查询登记状态…" : "登记状态尚未核实"}</p>
            ) : (
              <>
                <div className={styles.name_field}>
                <label htmlFor="host-registration-name">设备名称</label>
                {canApply ? (
                  <TextInput
                    id="host-registration-name"
                    placeholder="设备名称"
                    value={store.name}
                    disabled={store.pending || store.uncertain}
                    onChange={(event) => store.setName(event.target.value)}
                  />
                ) : (
                  <p>
                    {status.device?.name ?? "尚未核实"}
                    {state === "registered" && " · 由企业管理员管理"}
                  </p>
                )}
                </div>
                <dl className={styles.rows}>
                <div><dt>登记状态</dt><dd>{state ? labels[state] : status.registration_id ? "申请结果待核实" : "未登记"}
                  {state === "registered" && !status.device?.enabled && " · 已停用，请联系管理员启用"}
                </dd></div>
                <div><dt>连接状态</dt><dd><span className={styles.connection} data-online={status.connection === "online"}>{connections[status.connection]}</span></dd></div>
                </dl>
                {state === "registered" && (
                  <p className={styles.note}>
                    {grant === "allowed"
                      ? "你已获得本设备的企业组网访问权限。"
                      : grant === "missing"
                        ? "你尚未获得本设备的企业组网访问权限，请联系管理员分配；不影响本机使用及原有直连登录。"
                        : grant === "loading"
                          ? "正在核实当前账号的组网权限…"
                          : "当前账号的组网权限尚未核实，请刷新状态。"}
                  </p>
                )}
                {["exit", "withdraw"].includes(status.intent ?? "") && (
                  <p role="status">本机已停止目标接入，中心撤销待确认。</p>
                )}
                {canApply && <p className={styles.note}>管理员确认并分配用户后，获授权用户可通过企业组网访问这台电脑。</p>}
                <div className={styles.actions}>
                  {store.uncertain ? (
                    <Button disabled={store.loading || store.pending} onClick={() => void store.refresh()}>
                      查询结果
                    </Button>
                  ) : (
                    <>
                      {canApply && (
                        <Button
                          variant="primary"
                          disabled={
                            store.pending ||
                            store.loading ||
                            !store.name.trim() ||
                            store.name.trim().length > 80 ||
                            [...store.name].some(
                              (char) =>
                                char.charCodeAt(0) < 32 || (char.charCodeAt(0) >= 127 && char.charCodeAt(0) <= 159),
                            )
                          }
                          onClick={() => void store.submit({ type: "apply", payload: { name: store.name.trim() } })}
                        >
                          {store.pending ? "正在提交…" : status.registration_id ? "重新申请" : "申请登记"}
                        </Button>
                      )}
                      {state === "pending" && status.intent === "publish" && (
                        <Button disabled={store.pending || store.loading} onClick={() => setConfirm("withdraw")}>
                          撤回申请
                        </Button>
                      )}
                      {state === "registered" && status.intent === "publish" && (
                        <Button
                          variant="danger"
                          disabled={store.pending || store.loading}
                          onClick={() => setConfirm("exit")}
                        >
                          退出组网
                        </Button>
                      )}
                    </>
                  )}
                </div>
                <div className={styles.details}>
                  <CollapsibleSection title="设备信息">
                    <dl className={styles.rows}>
                      <div><dt>设备编号</dt><dd>{status.registration_id ?? "尚未登记"}</dd></div>
                      <div><dt>安装身份</dt><dd>{status.installation_id ?? "尚未建立"}</dd></div>
                      <div><dt>平台</dt><dd>{status.device?.platform ?? root.local_connection.capabilities?.platform ?? "未知"}</dd></div>
                      <div><dt>Host 版本</dt><dd>{status.device?.host_version ?? root.local_connection.capabilities?.runtime_version}</dd></div>
                    </dl>
                  </CollapsibleSection>
                </div>
              </>
            )}
          </section>
        </div>
      )}
      {confirm && (
        <Dialog
          aria_label={confirm === "exit" ? "退出组网" : "撤回申请"}
          backdrop_class_name={styles.backdrop}
          dialog_class_name={styles.dialog}
          dismissible={!store.pending}
          on_close={() => setConfirm(null)}
        >
          <h3>{confirm === "exit" ? "退出组网？" : "撤回这台电脑的登记申请？"}</h3>
          {confirm === "exit" && (
            <p>
              退出后，其他设备将无法通过企业组网访问这台电脑，已有组网访问会中断。本机文件、会话和任务保留，原有直连登录不变。
            </p>
          )}
          <div className={styles.actions}>
            <Button disabled={store.pending} onClick={() => setConfirm(null)}>
              取消
            </Button>
            <Button
              variant="danger"
              disabled={store.pending}
              onClick={() => {
                const command = confirm;
                setConfirm(null);
                void store.submit({ type: command });
              }}
            >
              {confirm === "exit" ? "退出组网" : "撤回申请"}
            </Button>
          </div>
        </Dialog>
      )}
    </SettingsPageContainer>
  );
});
