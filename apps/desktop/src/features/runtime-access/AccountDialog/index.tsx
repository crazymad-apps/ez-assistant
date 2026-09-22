import { useEffect, useId, useState } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import styles from "./index.module.scss";

export const AccountDialog = observer(function AccountDialog({ connection }: Readonly<{ connection: ApplicationConnectionStore }>) {
  const password_form_id = useId();
  const [old_password, setOld] = useState("");
  const [new_password, setNew] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  useEffect(() => { setOld(""); setNew(""); setMessage(""); }, [connection.session?.login_context, connection.password_open]);
  const logout = connection.logout_confirmation;
  const title = logout ? "退出登录" : "修改密码";
  const close = () => {
    if (busy || connection.pending) return;
    setOld(""); setNew(""); setMessage("");
    connection.cancelLogout(); connection.showPassword(false);
  };
  async function changePassword() {
    if (busy || connection.pending) return;
    const owner = connection.session?.login_context;
    setBusy(true); setMessage("");
    try {
      await connection.changePassword(old_password, new_password);
      if (owner === connection.session?.login_context) setMessage("密码已修改，已有登录和运行中任务保持有效。");
    } catch (error) { if (owner === connection.session?.login_context) setMessage(error instanceof Error ? error.message : "修改密码失败。"); }
    finally { if (owner === connection.session?.login_context) { setOld(""); setNew(""); } setBusy(false); }
  }
  return <Dialog open={connection.password_open || logout} aria_label={logout ? "确认退出登录" : "修改密码"} backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog} on_close={close} dismissible={!busy && !connection.pending}>
    <header><h3>{title}</h3></header>
    <div className={styles.body}>
      <p>{connection.account_label} · {connection.host_label}</p>
      {logout ? <p className={styles.impact}>退出将中断此账号在当前 Host 的所有任务，并使此账号的所有客户端退出登录。其他用户不受影响。</p> : <>
        {connection.session?.identity && <form id={password_form_id} className={styles.form} onSubmit={(event) => { event.preventDefault(); void changePassword(); }}>
          <label htmlFor="account-old-password">当前密码</label>
          <input id="account-old-password" type="password" autoComplete="current-password" placeholder="输入当前密码" value={old_password} onChange={(event) => setOld(event.target.value)} required />
          <label htmlFor="account-new-password">新密码</label>
          <input id="account-new-password" type="password" autoComplete="new-password" placeholder="输入新密码" value={new_password} onChange={(event) => setNew(event.target.value)} required />
        </form>}
      </>}
      {message && <p role="status">{message}</p>}
      {connection.error && <p role="alert">{connection.error}</p>}
    </div>
    <footer><Button disabled={busy || connection.pending} onClick={close}>{logout ? "取消" : "关闭"}</Button>
      {logout ? <Button variant="danger" disabled={connection.pending} onClick={() => void connection.signOut()}>{connection.pending ? "正在退出…" : "中断所有任务并退出"}</Button> : null}
      {/* footer 按钮关联原生表单，保留必填校验和 Enter 提交。 */}
      {!logout && connection.session?.identity && <Button type="submit" form={password_form_id} variant="primary" disabled={busy || connection.pending || !old_password || !new_password}>{busy ? "正在修改…" : "修改密码"}</Button>}
    </footer>
  </Dialog>;
});
