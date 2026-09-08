import { isTauri } from "@tauri-apps/api/core";
import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import { Button } from "../../../components/Button";
import { SelectionPopover } from "../../../components/SelectionPopover";
import type { HostAccessConfiguration, HostAccessStatus } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SettingsPageContainer } from "../../settings/SettingsDialog/SettingsPageContainer";
import styles from "./index.module.scss";

export const HostAccessSettings = observer(function HostAccessSettings({ onDirtyChange, on_back }: Readonly<{ onDirtyChange: (dirty: boolean) => void; on_back: () => void }>) {
  const root = useRootStore();
  const client = root.connection.state === "connected" ? root.runtime_client : null;
  const [status, setStatus] = useState<HostAccessStatus | null>(null);
  const [draft, setDraft] = useState<HostAccessConfiguration | null>(null);
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [scheme_open, setSchemeOpen] = useState(false);

  useEffect(() => {
    let active = true;
    if (client) void client.hostAccessCommand({ type: "get_status" }).then((value) => {
      if (active) { setStatus(value); setDraft(value.configuration); }
    }).catch((error: unknown) => { if (active) setError(displayError(error)); });
    return () => { active = false; onDirtyChange(false); };
  }, [client, onDirtyChange]);

  function update(next: Partial<HostAccessConfiguration>) {
    if (draft) setDraft({ ...draft, ...next });
    onDirtyChange(true);
    setNotice(null);
  }

  async function refresh() {
    if (!client) return;
    setPending(true); setError(null);
    try {
      const next = await client.hostAccessCommand({ type: "get_status" });
      setStatus(next); setDraft(next.configuration); setPassword(""); onDirtyChange(false);
    } catch (error) { setError(displayError(error)); }
    finally { setPending(false); }
  }

  async function save(kind: "password" | "configuration") {
    if (!client || !status || !draft) return;
    setPending(true); setError(null); setNotice(null);
    const submitted = password;
    if (kind === "password") setPassword("");
    try {
      const next = await client.hostAccessCommand(kind === "password"
        ? { type: "set_password", payload: { expected_revision: status.revision, password: submitted } }
        : { type: "configure", payload: { expected_revision: status.revision, configuration: { ...draft, server_names: draft.server_names.map((name) => name.trim().toLowerCase()).filter(Boolean) } } });
      setStatus(next);
      if (kind === "configuration") setDraft(next.configuration);
      onDirtyChange(kind === "password" ? JSON.stringify(draft) !== JSON.stringify(next.configuration) : password.length > 0);
      setNotice(kind === "password" ? "密码已更新，已有普通登录已失效。" : next.restart_required ? "已保存，重启 Host 后端口、协议和证书生效。" : "访问设置已保存。");
    } catch (error) { setError(displayError(error)); }
    finally { setPending(false); }
  }

  const locked = status?.listener_state === "listening";
  const closing_current = locked && draft?.remote_enabled === false && !isLoopbackAddress(client?.address);
  return <SettingsPageContainer title="访问设置" on_back={on_back} back_label="返回 Runtime" actions={<Button disabled={pending} onClick={() => void refresh()} variant="text">重新载入</Button>}>
    {!status || !draft ? <p role="status">{error ?? "正在读取访问设置…"}</p> : <div className={styles.settings}>
      <section className={styles.card}>
        <h4>访问密码</h4>
        <p>{status.password_configured ? "已设置密码，可直接修改。" : "先设置密码，再允许非本机客户端访问。"}</p>
        <label htmlFor="new-host-password">新密码</label>
        <div className={styles.password_row}>
          <input autoComplete="new-password" disabled={pending} id="new-host-password" onChange={(event) => { setPassword(event.target.value); onDirtyChange(true); }} placeholder="输入新密码" type="password" value={password} />
          <Button disabled={pending || !password.trim() || new TextEncoder().encode(password).length > 1024} onClick={() => void save("password")}>保存密码</Button>
        </div>
      </section>
      <section className={styles.card}>
        <div className={styles.heading}><h4>非本地访问</h4><span>{locked ? "已开启" : "已关闭"}</span></div>
        <label className={styles.toggle}><input checked={draft.remote_enabled} disabled={pending || !status.password_configured || status.restart_required} onChange={(event) => update({ remote_enabled: event.target.checked })} type="checkbox" />允许其他设备连接</label>
        <p>本机与其他设备共用同一个端口，使用这台电脑的 IP 直接访问。</p>
        <label htmlFor="host-server-names">允许的域名（可选，每行一个）</label>
        <textarea disabled={pending} id="host-server-names" onChange={(event) => update({ server_names: event.target.value.split("\n") })} placeholder="assistant.example.com" rows={2} value={draft.server_names.join("\n")} />
        <p>仅使用 IP 时留空；域名无需填写协议和端口。</p>
        {closing_current && <p className={styles.warning}>关闭后，当前连接将中断。Host 上的任务继续运行，可通过本机设置重新开启。</p>}
      </section>
      <section className={styles.card}>
        <h4>Host 服务</h4>
        <p>当前连接：{client?.address}</p>
        <fieldset disabled={pending || locked}>
          <label>协议</label>
          <SelectionPopover aria_label="访问协议" disabled={pending || locked} open={scheme_open} on_open_change={setSchemeOpen} on_select={(scheme) => update({ scheme })} options={[{ value: "http", label: "HTTP" }, { value: "https", label: "HTTPS" }]} selected={draft.scheme} trigger_variant="field" />
          <label htmlFor="host-port">端口</label>
          <input id="host-port" inputMode="numeric" max={65535} min={1} type="number" onChange={(event) => update({ port: Number(event.target.value) })} placeholder="7240" value={draft.port || ""} />
          {draft.scheme === "https" && <>
            <p>HTTPS 证书需覆盖 127.0.0.1 及实际访问的 IP／域名，并受客户端信任。</p>
            <label htmlFor="host-certificate">证书路径（Host）</label><input id="host-certificate" onChange={(event) => update({ tls_certificate: event.target.value || null })} placeholder="/absolute/path/host.crt" value={draft.tls_certificate ?? ""} />
            <label htmlFor="host-private-key">私钥路径（Host）</label><input id="host-private-key" onChange={(event) => update({ tls_private_key: event.target.value || null })} placeholder="/absolute/path/host.key" value={draft.tls_private_key ?? ""} />
          </>}
        </fieldset>
        {locked ? <p>关闭非本地访问后，可修改端口、协议和证书。</p> : <p>修改端口、协议或证书后需重启 Host；访问开关和域名立即生效。</p>}
        {status.restart_required && <p role="status" className={styles.warning}>端口、协议或证书已修改，请重启 Host 后再开启非本地访问。</p>}

        {status.restart_required && isTauri() && root.desktop_lifecycle.local_impact_known && <Button disabled={pending || root.desktop_lifecycle.pending} onClick={() => root.desktop_lifecycle.request("restart_runtime")}>重启本机 Runtime</Button>}
        <Button disabled={pending || !Number.isInteger(draft.port) || draft.port < 1 || draft.port > 65535} onClick={() => void save("configuration")} variant="primary">{pending ? "保存中…" : "保存访问设置"}</Button>
      </section>
      {(status.error || error) && <p className={styles.error} role="alert">{error ?? status.error}</p>}
      {notice && <p role="status">{notice}</p>}
    </div>}
  </SettingsPageContainer>;
});

function displayError(error: unknown): string { return error instanceof Error ? error.message : "无法保存 Host 访问设置。"; }

function isLoopbackAddress(address: string | undefined): boolean {
  if (!address) return false;
  try { return ["127.0.0.1", "localhost"].includes(new URL(address).hostname); }
  catch { return false; }
}
