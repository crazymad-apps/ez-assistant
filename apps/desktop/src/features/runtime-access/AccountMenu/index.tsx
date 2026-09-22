import { observer } from "mobx-react-lite";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../../../components/DropdownMenu";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";

/** 改密进入独立表单；退出先确认任务中断影响。 */
export const AccountMenu = observer(function AccountMenu({ connection }: Readonly<{ connection: ApplicationConnectionStore }>) {
  return <DropdownMenu>
    <DropdownMenuTrigger variant="text" disabled={connection.pending}>{connection.account_label}</DropdownMenuTrigger>
    <DropdownMenuContent aria-label="账号菜单" align="end">
      {connection.session?.identity && <DropdownMenuItem onSelect={() => connection.showPassword()}>修改密码</DropdownMenuItem>}
      <DropdownMenuItem onSelect={() => connection.requestLogout()}>退出登录</DropdownMenuItem>
    </DropdownMenuContent>
  </DropdownMenu>;
});
