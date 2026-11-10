import { FeedbackPanel } from "../../feedback-center/FeedbackPanel";
import { observer } from "mobx-react-lite";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import { OrganizationMenu } from "./OrganizationMenu";
import styles from "./index.module.scss";

/** 改密进入独立表单；退出先确认任务中断影响。 */
export const AccountMenu = observer(function AccountMenu({ connection, sidebar = false }: Readonly<{
  connection: ApplicationConnectionStore;
  sidebar?: boolean;
}>) {
  const store = connection.current;
  const connected = store?.local_connection.state === "connected";
  const identity = connection.session?.identity;
  const organization = connection.phase === "workspace" ? identity : null;
  const connection_status = connected ? "Runtime 已连接" : "Runtime 未连接";
  const sidebar_menu_width = sidebar && store
    ? `calc(min(${store.navigation.left_sidebar_width}px, 100vw - 56px) - 24px)`
    : undefined;

  return <><div className={styles.account_footer} role="group" aria-label="当前账号">
    <DropdownMenu className={styles.account_root}>
      <DropdownMenuTrigger aria-label="账号与 Runtime" aria-description={connection_status} className={styles.account_trigger} disabled={connection.pending && connection.phase !== "workspace"} title={connection.account_label}>
        <span className={styles.avatar} aria-hidden="true"><Icon name="user" size={17} /></span>
        <span className={styles.account_copy}>
          <strong className={styles.account_name}>{connection.account_label}</strong>
          <small><i className={styles.status_dot} data-connected={connected} />{connection_status}</small>
        </span>
        <Icon name={connection.phase === "workspace" ? "settings" : "chevron-down"} size={17} />
      </DropdownMenuTrigger>
      <DropdownMenuContent aria-label="账号菜单" align="start"
        className={`${styles.account_menu} ${sidebar ? styles.sidebar_account_menu : ""}`}
        style={sidebar_menu_width ? { width: sidebar_menu_width } : undefined}>
        <div className={styles.account_summary}>
          <div className={styles.summary_heading}><strong>{connection.account_label}</strong>{connection.updates.visible && <DropdownMenuItem className={styles.update_button} aria-label="更新应用" onSelect={() => connection.updates.show()}><Icon name="refresh" size={13} />更新</DropdownMenuItem>}</div>
          {organization && <OrganizationMenu key={organization.organization_id} connection={connection} identity={organization} />}
        </div>
        {connection.phase === "workspace" && <DropdownMenuItem disabled={!store} onSelect={() => { if (connection.phase === "workspace") store?.settings.open(); }}><Icon name="settings" size={15} />设置</DropdownMenuItem>}
        {identity && store && <DropdownMenuItem disabled={!connected} onSelect={() => store.feedback_center.show()}><Icon name="message" size={15} />反馈中心</DropdownMenuItem>}
        {identity && <DropdownMenuItem disabled={connection.pending} onSelect={() => connection.showPassword()}><Icon name="shield" size={15} />修改密码</DropdownMenuItem>}
        {connection.desktop && <DropdownMenuItem disabled={connection.web_pending || !connected} onSelect={() => void connection.openWeb()}><Icon name="globe" size={15} />打开 Web 端</DropdownMenuItem>}
        {store && (store.local_connection.state === "disconnected" || store.local_connection.state === "component_mismatch" || store.local_connection.state === "runtime_stopped") && (
          <DropdownMenuItem onSelect={() => store.retryLocalConnection()}>重新连接</DropdownMenuItem>
        )}
        <DropdownMenuItem disabled={connection.pending} onSelect={() => connection.requestLogout()}><Icon name="logout" size={15} />退出登录</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  </div>{store && <FeedbackPanel key={connection.session?.login_context} store={store.feedback_center} />}</>;
});
