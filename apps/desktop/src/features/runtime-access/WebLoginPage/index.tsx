import { observer } from "mobx-react-lite";
import { useState, type FormEvent } from "react";
import { AccountMenu } from "../AccountMenu";
import { Button } from "../../../components/Button";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import { RuntimeEntryLayout } from "../RuntimeEntryLayout";
import styles from "./index.module.scss";

type WebLoginPageProps = {
  readonly connection: ApplicationConnectionStore;
};

export const WebLoginPage = observer(function WebLoginPage({
  connection,
}: WebLoginPageProps) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  async function submit(event: FormEvent) {
    event.preventDefault();
    const submitted = password;
    setPassword("");
    await connection.login(submitted, username);
  }
  return (
    <RuntimeEntryLayout>
      <section className={styles.login} aria-label="Host 登录">
        {connection.phase !== "login" ? (
          <div className={styles.form}>
            <p className={styles.loading} role="status">{connection.error ?? connection.progress ?? "正在连接 Runtime…"}</p>
            {connection.can_retry && connection.error && <Button disabled={connection.pending} onClick={() => void connection.retryInitialization()}>重试初始化</Button>}
            {connection.session && <AccountMenu connection={connection} />}
          </div>
        ) : (
          <form
            className={styles.form}
            onSubmit={(event) => void submit(event)}
          >
            {connection.mode === "enterprise" && <div className={styles.field}>
              <label htmlFor="host-username">企业账号</label>
              <input id="host-username" autoComplete="username" placeholder="输入企业账号" value={username} onChange={(event) => setUsername(event.target.value)} required />
            </div>}
            <div className={styles.field}>
              <label htmlFor="host-password">{connection.mode === "enterprise" ? "账号密码" : "访问密码"}</label>
              <input
                autoComplete="current-password"
                autoFocus
                id="host-password"
                onChange={(event) => setPassword(event.target.value)}
                placeholder={connection.mode === "enterprise" ? "输入账号密码" : "输入 Host 密码"}
                type="password"
                value={password}
              />
            </div>
            <Button
              className={styles.submit}
              disabled={connection.pending || !password.trim()}
              size="large"
              type="submit"
              variant="primary"
            >
              {connection.pending ? "连接中…" : "进入工作空间"}
              <span aria-hidden="true">→</span>
            </Button>
            <div className={styles.feedback} aria-live="polite">
              {connection.warning && <p role="status">{connection.warning}</p>}
              {connection.error && <p role="alert">{connection.error}</p>}
            </div>
          </form>
        )}
      </section>
    </RuntimeEntryLayout>
  );
});
