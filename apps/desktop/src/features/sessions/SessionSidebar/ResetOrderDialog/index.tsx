import { useEffect, useRef } from "react";
import { observer } from "mobx-react-lite";
import type { ResetSidebarOrderRequest } from "@ez-assistant/protocol";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../SessionActionDialog";

export const ResetOrderDialog = observer(function ResetOrderDialog({ scope, title, on_close }: Readonly<{
  scope: ResetSidebarOrderRequest;
  title: string;
  on_close: () => void;
}>) {
  const order = useRootStore().sidebar_order;

  const owner = order.key(scope);
  const initial_owner = useRef(owner);
  useEffect(() => {
    if (owner !== initial_owner.current) on_close();
  }, [owner, on_close]);

  async function confirm() {
    if (await order.reset(scope)) on_close();
  }

  return <SessionActionDialog title={title} confirm_label="恢复默认" is_pending={order.isPending(scope)} confirm_disabled={!order.enabled(scope)} on_cancel={on_close} on_confirm={() => void confirm()}>
    <p>将按创建时间重新排列，新建的在前。手动调整的顺序无法找回。</p>
    {order.error(scope) && <p role="alert">{order.error(scope)}</p>}
  </SessionActionDialog>;
});
