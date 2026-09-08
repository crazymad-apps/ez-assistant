import { observer } from "mobx-react-lite";
import { useState, type FormEvent } from "react";
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
  const [password, setPassword] = useState("");
  async function submit(event: FormEvent) {
    event.preventDefault();
    const submitted = password;
    setPassword("");
    await connection.login(submitted);
  }
  return (
    <RuntimeEntryLayout description={window.location.host}>
      <section className={styles.login} aria-label="Host 登录">
        {connection.phase !== "login" ? (
          <p className={styles.loading} role="status">
            正在连接 Runtime…
          </p>
        ) : (
          <form
            className={styles.form}
            onSubmit={(event) => void submit(event)}
          >
            <div className={styles.field}>
              <label htmlFor="host-password">访问密码</label>
              <input
                autoComplete="current-password"
                autoFocus
                id="host-password"
                onChange={(event) => setPassword(event.target.value)}
                placeholder="输入 Host 密码"
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
              {connection.error && <p role="alert">{connection.error}</p>}
            </div>
          </form>
        )}
      </section>
    </RuntimeEntryLayout>
  );
});
