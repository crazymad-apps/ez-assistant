import { useRef, useState, useEffect } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import { Icon } from "../../../components/Icon";
import { getDesktopPlatform } from "../../../native-bridge/desktopLifecycle";
import { rememberedRuntimePassword } from "../../../native-bridge/runtimeConnection";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import styles from "./index.module.scss";

export const RuntimeConnectionForm = observer(function RuntimeConnectionForm({ connection, settings = false }: Readonly<{ connection: ApplicationConnectionStore; settings?: boolean }>) {
  const [origin, setOrigin] = useState(connection.address);
  const [password, setPassword] = useState("");
  const [remember, setRemember] = useState(false);
  const [keychain, setKeychain] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const read_owner = useRef(0);
  const current = connection.current?.connection;
  const local_failed = connection.target === "local" && connection.local_state === "failed";
  const error = connection.error || (local_failed ? connection.local_error : null);
  let normalized_origin = "";
  try { normalized_origin = new URL(origin.trim()).origin; } catch { /* 表单尚未填完。 */ }
  const already_connected = settings && current?.state === "connected"
    && connection.target === connection.connected_target
    && (connection.target === "local" || normalized_origin === current.address);
  useEffect(() => { let active = true; void getDesktopPlatform().then((platform) => { if (active) setKeychain(platform === "macos"); }); return () => { active = false; read_owner.current += 1; }; }, []);
  async function readPassword() {
    if (!keychain || password || !origin.trim()) return;
    const owner = ++read_owner.current;
    try {
      const value = await rememberedRuntimePassword(origin);
      if (owner !== read_owner.current) return;
      if (value) { setPassword(value); setRemember(true); }
    } catch { if (owner === read_owner.current) setNotice("未能读取记住的密码，请手动输入。"); }
  }
  return <form className={styles.form} onSubmit={(event) => { event.preventDefault(); read_owner.current += 1; void connection.connectDesktop(connection.target === "local" ? null : origin.trim(), password, remember); }}>
    <div className={styles.selector} role="group" aria-label="Runtime 目标">
      <button type="button" aria-pressed={connection.target === "local"} onClick={() => connection.chooseTarget("local")}><Icon name="terminal" size={17} />本机 Runtime</button>
      <button type="button" aria-pressed={connection.target === "remote"} onClick={() => connection.chooseTarget("remote")}><Icon name="globe" size={17} />其他 Runtime</button>
    </div>
    <div className={styles.target}>
      {connection.target === "local" ? <div className={styles.local}>
        <span className={styles.device}><Icon name="terminal" size={21} /></span>
        <div><strong>这台电脑</strong><p>{connection.local_state === "ready" ? "本机 Runtime 已就绪" : connection.local_state === "starting" ? "正在静默启动 Runtime…" : "本机 Runtime 启动失败"}</p></div>
        <span className={styles.status} data-ready={connection.local_state === "ready"} />
      </div> : <div className={styles.remote}>
        <label htmlFor="runtime-origin">Host 地址</label>
        <input id="runtime-origin" autoComplete="url" placeholder="http://192.168.1.20:7240" value={origin} onBlur={() => void readPassword()} onChange={(event) => { read_owner.current += 1; setOrigin(event.target.value); setPassword(""); setRemember(false); setNotice(null); }} required />
        <label htmlFor="runtime-password">访问密码</label>
        <input id="runtime-password" type="password" autoComplete="off" value={password} onChange={(event) => { read_owner.current += 1; setPassword(event.target.value); }} required />
        {keychain && <label className={styles.remember}><input type="checkbox" checked={remember} onChange={(event) => setRemember(event.target.checked)} />记住密码</label>}
      </div>}
    </div>
    {settings && <p className={styles.hint}>切换将丢弃未发送草稿，并关闭当前客户端的临时资源。</p>}
    <Button className={styles.submit} size="large" variant="primary" type="submit" disabled={already_connected || connection.pending || (connection.target === "remote" && (!origin.trim() || !password))}>{connection.pending ? connection.progress : already_connected ? "当前连接" : settings ? "切换 Runtime" : connection.target === "local" ? "进入工作空间" : "连接 Runtime"}<span aria-hidden="true">→</span></Button>
    <div className={styles.feedback} aria-live="polite">
      {error && <p className={styles.error} role="alert">{error}</p>}
      {local_failed && <Button size="small" onClick={() => void connection.startLocal().catch(() => undefined)}>重试启动</Button>}
      {notice && <p>{notice}</p>}
      {connection.warning && <p>{connection.warning}</p>}
    </div>
  </form>;
});
