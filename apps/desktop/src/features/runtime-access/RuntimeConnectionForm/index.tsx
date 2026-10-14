import { useRef, useState, useEffect } from "react";
import { observer } from "mobx-react-lite";
import { AccountMenu } from "../AccountMenu";
import { Button } from "../../../components/Button";
import { Icon } from "../../../components/Icon";
import { getDesktopPlatform } from "../../../native-bridge/desktopLifecycle";
import { rememberedRuntimePassword } from "../../../native-bridge/runtimeConnection";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import styles from "./index.module.scss";

export const RuntimeConnectionForm = observer(function RuntimeConnectionForm({ connection, settings = false }: Readonly<{ connection: ApplicationConnectionStore; settings?: boolean }>) {
  const [origin, setOrigin] = useState(connection.address);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(false);
  const [keychain, setKeychain] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const read_owner = useRef(0);
  const form_ref = useRef<HTMLFormElement>(null);
  const focus_restored = useRef(false);
  const current = connection.current?.connection;
  const local_failed = connection.target === "local" && connection.local_state === "failed";
  const local_upgrade = connection.target === "local" && connection.local_state === "upgrade_required";
  const update_required = connection.update_required;
  const error = connection.error || (local_failed || local_upgrade ? connection.local_error : null);
  let normalized_origin = "";
  try { normalized_origin = new URL(origin.trim()).origin; } catch { /* 表单尚未填完。 */ }
  // 企业模式可在同一 Host 切换账号，不能仅因地址相同禁用提交。
  const already_connected = settings && connection.mode !== "enterprise" && current?.state === "connected"
    && connection.target === connection.connected_target
    && (connection.target === "local" || normalized_origin === current.address);
  const missing_credentials = connection.mode === "enterprise" && (!username.trim() || !password);
  const submit_disabled = update_required || connection.updates.downloading || connection.updates.installing || connection.pending || connection.login_options_busy || (connection.target === "local" && connection.local_state === "starting")
    || (!local_upgrade && (already_connected || missing_credentials || (connection.target === "remote" && (!origin.trim() || !password))));
  let submit_label = connection.target === "local" ? "进入项目" : "连接 Runtime";
  if (settings) submit_label = connection.mode === "enterprise" ? "确认切换" : "切换 Runtime";
  if (already_connected) submit_label = "当前连接";
  if (local_upgrade) submit_label = "更新并重启";
  if (connection.pending) submit_label = current?.error_message ?? connection.progress;
  let local_label = "本机 Runtime 启动失败";
  if (connection.local_state === "ready") local_label = "本机 Runtime 可连接";
  if (connection.local_state === "starting") local_label = "正在启动 Runtime，首次更新可能需要几分钟…";
  if (local_upgrade) local_label = "本机 Runtime 待更新";
  useEffect(() => { let active = true; void getDesktopPlatform().then((platform) => { if (active) setKeychain(platform === "macos"); }); return () => { active = false; read_owner.current += 1; }; }, []);
  useEffect(() => { setOrigin(connection.address); }, [connection.address]);
  useEffect(() => { read_owner.current += 1; setPassword(""); setUsername(""); setRemember(false); }, [connection.credentials_epoch]);
  useEffect(() => {
    focus_restored.current = false;
  }, [connection.login_options_notice, connection.credentials_epoch]);
  useEffect(() => {
    if (settings || !connection.login_options_notice || focus_restored.current || (connection.mode === "personal" && submit_disabled)) return;
    const frame = requestAnimationFrame(() => {
      const form = form_ref.current;
      const target = form?.querySelector<HTMLElement>(connection.mode === "enterprise" ? "#runtime-username" : 'button[type="submit"]');
      if (!form || !target) return;
      // 每次应用登录选项只恢复一次焦点；输入和连接状态变化不能抢走用户的焦点。
      focus_restored.current = true;
      if (!form.contains(document.activeElement)) target.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [connection.login_options_notice, connection.credentials_epoch, connection.mode, submit_disabled, settings]);
  async function readPassword() {
    if (!keychain || password || !origin.trim()) return;
    const owner = ++read_owner.current;
    try {
      const value = await rememberedRuntimePassword(origin);
      if (owner !== read_owner.current) return;
      if (value) { setPassword(value); setRemember(true); }
    } catch { if (owner === read_owner.current) setNotice("未能读取记住的密码，请手动输入。"); }
  }
  return <form ref={form_ref} className={styles.form} onSubmit={(event) => {
    event.preventDefault();
    if (submit_disabled) return;
    read_owner.current += 1;
    if (local_upgrade) { void connection.upgradeLocal().catch(() => undefined); return; }
    void connection.connectDesktop(connection.target === "local" ? null : origin.trim(), password, remember, username);
  }}>
    <div className={styles.selector} role="group" aria-label="Runtime 目标">
      <button type="button" disabled={connection.updates.installing || connection.login_options_busy} aria-pressed={connection.target === "local"} onClick={() => connection.chooseTarget("local")}><Icon name="terminal" size={17} />本机 Runtime</button>
      <button type="button" disabled={connection.updates.installing || connection.login_options_busy} aria-pressed={connection.target === "remote"} onClick={() => connection.chooseTarget("remote")}><Icon name="globe" size={17} />其他 Runtime</button>
    </div>
    <div className={styles.target}>
      {connection.target === "local" ? <div className={styles.local}>
        <span className={styles.device}><Icon name="terminal" size={21} /></span>
        <div className={styles.local_copy}><strong>这台电脑</strong><p><span className={styles.status} data-ready={connection.local_state === "ready"} />{local_label}</p></div>
        {!settings && connection.desktop && connection.phase !== "workspace" && <Button className={styles.options} variant="text" disabled={connection.pending} onClick={() => void connection.openLoginOptions()}><Icon name="settings" size={15} />登录选项</Button>}
      </div> : <div className={styles.remote}>
        <label htmlFor="runtime-origin">Host 地址</label>
        <input disabled={connection.updates.installing || connection.login_options_busy} id="runtime-origin" autoComplete="url" placeholder="http://192.168.1.20:7240" value={origin} onBlur={() => { void connection.discoverTarget(origin); void readPassword(); }} onChange={(event) => { read_owner.current += 1; setOrigin(event.target.value); setPassword(""); setRemember(false); setNotice(null); }} required />
      </div>}
      {!settings && connection.login_options_notice && <p className={styles.hint} role="status">{connection.login_options_notice}</p>}
      {!update_required && !local_upgrade && (connection.target === "remote" || connection.mode === "enterprise") && <div className={styles.remote}>
        {connection.mode === "enterprise" && <><label htmlFor="runtime-username">企业账号</label><input id="runtime-username" autoComplete="username" placeholder="输入企业账号" value={username} onChange={(event) => setUsername(event.target.value)} required /></>}
        <label htmlFor="runtime-password">{connection.mode === "enterprise" ? "账号密码" : "访问密码"}</label>
        <input placeholder="请输入" id="runtime-password" type="password" autoComplete="off" value={password} onChange={(event) => { read_owner.current += 1; setPassword(event.target.value); }} required />
        {keychain && connection.mode !== "enterprise" && connection.target === "remote" && <label className={styles.remember}><input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />记住密码</label>}
      </div>}
    </div>
    {update_required && <div className={styles.update} role="status">
      <strong>需要更新后继续使用</strong>
      {!connection.updates.visible && <p>暂时无法更新，请稍后重试。</p>}
      {connection.updates.visible && <Button variant="primary" size="large" onClick={() => connection.updates.show()}>{connection.updates.installing ? "正在更新" : connection.updates.downloading ? "查看更新进度" : connection.updates.snapshot.available ? "更新并重启" : "查看更新状态"}</Button>}
      {!connection.updates.downloading && !connection.updates.installing && <Button disabled={connection.pending} onClick={() => void connection.retryUpdateConnection()}>重新连接</Button>}
    </div>}
    {!update_required && connection.can_retry && connection.error && <Button disabled={connection.pending} onClick={() => void connection.retryInitialization()}>重试初始化</Button>}
    {!update_required && !settings && connection.session && <AccountMenu connection={connection} />}
    {settings && <p className={styles.hint}>切换将丢弃未发送草稿，并关闭当前客户端的临时资源。</p>}
    {!update_required && <Button className={styles.submit} size="large" variant="primary" type="submit" disabled={submit_disabled}>{submit_label}<span aria-hidden="true">→</span></Button>}
    <div className={styles.feedback} aria-live="polite">
      {!update_required && error && <p className={styles.error} role="alert">{error}</p>}
      {!update_required && local_failed && <Button size="small" disabled={Boolean(connection.login_options)} onClick={() => void connection.startLocal().catch(() => undefined)}>重试启动</Button>}
      {!update_required && notice && <p>{notice}</p>}
      {!update_required && connection.warning && <p>{connection.warning}</p>}
    </div>
  </form>;
});
