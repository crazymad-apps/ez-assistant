import { useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import type { ListHostFilesResult } from "../../../generated/assistant-protocol";
import styles from "./index.module.scss";

export const HostDirectoryDialog = observer(function HostDirectoryDialog() {
  const root = useRootStore();
  const [target, setTarget] = useState(
    root.directory_picker?.initial_path ?? null,
  );
  const [path, setPath] = useState(target ?? "");
  const [hidden, setHidden] = useState(false);
  const [listing, setListing] = useState<ListHostFilesResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [selecting, setSelecting] = useState(false);
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    const abort = new AbortController();
    setLoading(true);
    setError(null);
    void root.files
      .listHostFiles(target, hidden, abort.signal)
      .then((result) => {
        if (abort.signal.aborted) return;
        setListing(result);
        setPath(result.path);
      })
      .catch((failure: unknown) => {
        if (!abort.signal.aborted)
          setError(
            failure instanceof Error ? failure.message : "目录读取失败。",
          );
      })
      .finally(() => {
        if (!abort.signal.aborted) setLoading(false);
      });
    return () => abort.abort();
  }, [root, target, hidden, refresh]);
  async function select() {
    if (!listing || error || loading || selecting) return;
    const owner = root.directory_picker;
    setSelecting(true);
    try {
      const path = await root.files.selectHostDirectory(listing.path);
      if (root.directory_picker === owner) root.finishDirectorySelection(path);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : "目录不可用。");
    } finally {
      setSelecting(false);
    }
  }
  function go(value: string) {
    setPath(value);
    setTarget(value);
    setRefresh((n) => n + 1);
  }
  return (
    <Dialog
      aria_label="选择工作目录"
      dialog_class_name={styles.dialog}
      backdrop_class_name={styles.backdrop}
      on_close={() => root.finishDirectorySelection(null)}
    >
      <header>
        <div>
          <h3>选择工作目录</h3>
          <small>{root.files.address}</small>
        </div>
        <InlineIconButton
          icon="x"
          label="关闭目录选择"
          onClick={() => root.finishDirectorySelection(null)}
        />
      </header>
      <form
        className={styles.path}
        onSubmit={(event) => {
          event.preventDefault();
          go(path.trim());
        }}
      >
        <InlineIconButton
          icon="chevron-up"
          label="返回上级目录"
          disabled={!listing?.parent_path || loading}
          onClick={() => listing?.parent_path && go(listing.parent_path)}
        />
        <input
          aria-label="Host 目录路径"
          placeholder="输入 Host 上的绝对路径"
          value={path}
          onChange={(event) => setPath(event.currentTarget.value)}
        />
        <Button type="submit" disabled={!path.trim()}>
          前往
        </Button>
      </form>
      <label className={styles.hidden}>
        <input
          type="checkbox"
          checked={hidden}
          onChange={(event) => setHidden(event.currentTarget.checked)}
        />
        显示隐藏目录
      </label>
      <div
        className={styles.entries}
        aria-label="Host 目录"
        aria-busy={loading}
      >
        {loading ? (
          <p role="status">正在读取目录…</p>
        ) : error ? (
          <p role="alert">{error}</p>
        ) : (
          <>
            {listing?.entries
              .filter((entry) => entry.kind === "directory")
              .map((entry) => (
                <button
                  key={entry.path}
                  type="button"
                  disabled={entry.state !== "available"}
                  onClick={() => go(entry.path)}
                >
                  <Icon name="folder" size={17} />
                  <span>{entry.display_name}</span>
                  <Icon name="chevron-right" size={14} />
                </button>
              ))}
            {listing &&
              !listing.entries.some((entry) => entry.kind === "directory") && (
                <p>没有子目录，可选择当前目录。</p>
              )}
            {listing?.truncated && (
              <p role="status">
                目录较大，部分条目未显示；可输入完整路径前往。
              </p>
            )}
            {!!listing?.skipped_entries && (
              <p role="status">部分名称无法显示，可使用其他目录。</p>
            )}
          </>
        )}
      </div>
      <footer>
        <Button onClick={() => root.finishDirectorySelection(null)}>
          关闭
        </Button>
        <Button
          variant="primary"
          disabled={!listing || loading || !!error || selecting}
          onClick={() => void select()}
        >
          {selecting ? "正在确认…" : "选择此目录"}
        </Button>
      </footer>
    </Dialog>
  );
});
