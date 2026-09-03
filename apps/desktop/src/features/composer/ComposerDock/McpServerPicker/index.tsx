import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import type { ListMcpServerOptionsRequest, McpSelectionTagSnapshot } from "../../../../generated/assistant-protocol";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { InputContextPicker } from "../InputContextPicker";
import { McpServerPickerStore } from "./store";

/** 与技能选择复用搜索、键盘及焦点交互；由父级按 owner/variant 重新挂载。 */
export const McpServerPicker = observer(function McpServerPicker(props: Readonly<{
  request: ListMcpServerOptionsRequest;
  on_close: () => void;
  on_select: (server: McpSelectionTagSnapshot) => void;
}>) {
  const root = useRootStore();
  const [store] = useState(() => new McpServerPickerStore());
  const load = () => store.load(() => root.listMcpServerOptions(props.request));
  useEffect(() => {
    void load();
    return () => store.dispose();
  }, [store]);
  return <InputContextPicker
    empty_action={{ label: "前往 MCP 设置", on_select: () => {
      props.on_close();
      // 先恢复输入框焦点，再打开设置，避免关闭选择器的 rAF 抢走设置对话框焦点。
      requestAnimationFrame(() => root.settings.open("mcp"));
    } }}
    error={store.error}
    label="MCP 服务"
    loading={store.loading}
    on_close={props.on_close}
    on_retry={() => void load()}
    on_select={(server) => props.on_select({ server_key: server.server_key, display_name: server.display_name })}
    open
    option_label={(server) => `${server.display_name} (${server.server_key})`}
    options={store.servers.map((server) => ({ ...server, name: server.server_key }))}
  />;
});
