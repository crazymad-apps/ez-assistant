import type { SidebarScope } from "../../../stores/SidebarOrderStore";
import styles from "./index.module.scss";

export function SidebarLoadMore(props: Readonly<{
  scope: SidebarScope;
  loading: boolean;
  disabled?: boolean;
  onLoad: () => void;
}>) {
  return (
    <div data-sidebar-more={JSON.stringify(props.scope)}>
      <button
        className={styles.load_more}
        data-workspace-list={props.scope.scope === "workspaces" || undefined}
        disabled={props.loading || props.disabled}
        onClick={props.onLoad}
        type="button"
      >
        <span>{props.loading ? "加载中…" : "加载更多"}</span>
      </button>
    </div>
  );
}
