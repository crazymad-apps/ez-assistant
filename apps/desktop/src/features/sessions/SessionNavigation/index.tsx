import appLogo from "../../../../app-icon.svg";
import { observer } from "mobx-react-lite";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

/** 左栏与中栏只挂载其中一处；导航能力和按钮顺序保持一致。 */
export const SessionNavigation = observer(function SessionNavigation({ sidebar_open, on_toggle, on_search }: Readonly<{
  sidebar_open: boolean;
  on_toggle: () => void;
  on_search: () => void;
}>) {
  const store = useRootStore();
  const recent = [...(store.sidebar_projection.application?.active_sessions ?? [])]
    .sort((left, right) => (right.updated_at_ms ?? 0) - (left.updated_at_ms ?? 0))
    .slice(0, 8);

  return <nav aria-label="会话导航操作" className={styles.navigation} data-sidebar-open={sidebar_open}>
    <img alt="" aria-hidden="true" className={styles.app_logo} draggable={false} src={appLogo} />
    <InlineIconButton className={styles.icon_button} icon="sidebar-left" label={sidebar_open ? "收起会话栏" : "展开会话栏"} onClick={on_toggle} />
    <InlineIconButton className={styles.icon_button} icon="search" label="搜索会话" onClick={on_search} />
    <InlineIconButton className={styles.icon_button} disabled={!store.navigation.can_go_back} icon="chevron-left" label="返回上一处会话位置" onClick={() => void store.navigateBack()} />
    <InlineIconButton className={styles.icon_button} disabled={!store.navigation.can_go_forward} icon="chevron-right" label="前往下一处会话位置" onClick={() => void store.navigateForward()} />
    <DropdownMenu>
      <DropdownMenuTrigger aria-label="选择最近会话" className={styles.icon_button} inlineIcon="chevron-down" />
      <DropdownMenuContent align="start" aria-label="最近会话" className={styles.recent_menu}>
        {recent.map((item) => <DropdownMenuItem
          aria-current={store.navigation.target_device === store.network.browsing_device && item.session_id === store.navigation.selected_session_id ? "page" : undefined}
          key={item.session_id}
          onSelect={() => void store.selectSession(item.session_id, store.network.browsing_device)}
        >
          <span>{item.title}</span>
          {store.navigation.target_device === store.network.browsing_device && item.session_id === store.navigation.selected_session_id && <Icon name="check" size={14} />}
        </DropdownMenuItem>)}
        {recent.length === 0 && <span className={styles.recent_empty}>暂无会话</span>}
      </DropdownMenuContent>
    </DropdownMenu>
  </nav>;
});
