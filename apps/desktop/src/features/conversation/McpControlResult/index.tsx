import type { ConversationItem, McpServerRefreshOutcome } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import styles from "./index.module.scss";

const outcome_labels: Record<McpServerRefreshOutcome, string> = {
  refreshed: "已刷新",
  connected_without_tools: "已连接，无工具",
  retained_after_failure: "刷新失败，保留原状态",
  removed: "已移除",
  disabled: "已停用",
  not_found: "未找到服务",
};

/** 与普通用户气泡分离的可靠控制结果；不产生 Run、工具详情或重试模型按钮。 */
export function McpControlResult(props: Readonly<{
  message: Extract<ConversationItem, { type: "control_result" }>;
}>) {
  const result = props.message.result;
  const title = { success: "MCP 刷新完成", partial: "MCP 部分刷新完成", failure: "MCP 刷新失败" }[result.outcome];
  return (
    <article aria-label={title} className={styles.control_result} data-message-id={props.message.message_id} data-outcome={result.outcome}>
      <details>
        <summary><Icon name="refresh" size={14} /><span>{title}</span><small>{result.servers.length} 个服务</small></summary>
        <div className={styles.results}>
          {result.servers.length === 0 && <p>{result.outcome === "success" ? "当前没有需要刷新的 MCP 服务。" : "请检查 MCP 配置后重试，现有连接未更改。"}</p>}
          {result.servers.map((server) => (
            <div className={styles.server_result} key={server.server_key}>
              <strong>{server.server_key}</strong>
              <span>{outcome_labels[server.outcome]} · {server.tool_count} 个工具</span>
              {server.diagnostic && <p>{server.diagnostic.message}</p>}
            </div>
          ))}
        </div>
      </details>
    </article>
  );
}
