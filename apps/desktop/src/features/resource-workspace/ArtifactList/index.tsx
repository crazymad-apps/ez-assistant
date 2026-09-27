import { useEffect, useRef, useState } from "react";
import type { SessionArtifact, SessionId } from "@ez-assistant/protocol";
import { Icon } from "../../../components/Icon";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

type FileState = { available: boolean; error: string | null };

export function ArtifactList(props: Readonly<{ session_id: SessionId; artifacts: readonly SessionArtifact[]; refresh_key?: number }>) {
  const store = useRootStore();
  const [states, setStates] = useState<ReadonlyMap<string, FileState>>(new Map());
  const [refresh, setRefresh] = useState(0);
  const [pending, setPending] = useState<string | null>(null);
  const lifetime = useRef(0);
  // 父快照刷新可能高频替换数组；只在交付身份/标题确实变化时重新查询文件。
  const identity = JSON.stringify(props.artifacts);
  useEffect(() => {
    const controller = new AbortController();
    const generation = ++lifetime.current;
    setStates(new Map());
    const artifacts = JSON.parse(identity) as SessionArtifact[];
    void (async () => {
      // 顺序检查，避免较大清单耗尽 Host 的文件读取并发配额。
      for (const artifact of artifacts) {
        if (controller.signal.aborted) return;
        const key = JSON.stringify(artifact.locator);
        try {
          await store.files.statSessionResourceFile(props.session_id, artifact.locator, controller.signal);
          if (lifetime.current === generation) setStates((current) => new Map(current).set(key, { available: true, error: null }));
        } catch (error: unknown) {
          if (lifetime.current === generation) setStates((current) => new Map(current).set(key, {
            available: false, error: error instanceof Error ? error.message : "文件已失效或无法读取。",
          }));
        }
      }
    })();
    return () => { lifetime.current += 1; controller.abort(); };
  }, [store, props.session_id, identity, refresh, props.refresh_key]);

  useEffect(() => {
    const refreshFiles = () => setRefresh((value) => value + 1);
    window.addEventListener("focus", refreshFiles);
    return () => window.removeEventListener("focus", refreshFiles);
  }, []);

  async function operate(artifact: SessionArtifact, download: boolean) {
    const key = JSON.stringify(artifact.locator);
    if (pending) return;
    const generation = lifetime.current;
    setPending(key);
    try {
      await store.files.statSessionResourceFile(props.session_id, artifact.locator);
      if (lifetime.current !== generation) return;
      const name = artifact.locator.relative_path.split("/").at(-1) || artifact.title;
      if (download) await store.files.downloadSessionFile(props.session_id, artifact.locator, name);
      else store.resource_workspace.openSessionResource(`session:${props.session_id}`, props.session_id, artifact.locator, name);
    } catch (error: unknown) {
      if (lifetime.current !== generation) return;
      const message = error instanceof Error ? error.message : "无法打开或下载产物。";
      store.showInteractionError(message);
      // 下载取消/客户端保存失败不等于 Host 文件丢失；重新检查后再决定禁用状态。
      setRefresh((value) => value + 1);
    } finally {
      setPending(null);
    }
  }

  if (props.artifacts.length === 0) return <p className={styles.empty}>尚无产物</p>;
  return <div className={styles.list}>
    {props.artifacts.map((artifact) => {
      const key = JSON.stringify(artifact.locator);
      const state = states.get(key);
      return <div className={styles.item} key={key}>
        <button className={styles.open} disabled={!state?.available || pending !== null} type="button" onClick={() => void operate(artifact, false)}>
          <span className={styles.icon}><Icon name="file" size={16} /></span>
          <span className={styles.title} title={artifact.title}>{artifact.title}</span>
        </button>
        <InlineIconButton icon="arrow-down" label={`下载 ${artifact.title}`} disabled={!state?.available || pending !== null} onClick={() => void operate(artifact, true)} />
        {!state && <small className={styles.status}>正在检查…</small>}
        {state?.error && <p className={styles.status} role="status">文件不可用：{state.error}</p>}
      </div>;
    })}
  </div>;
}
