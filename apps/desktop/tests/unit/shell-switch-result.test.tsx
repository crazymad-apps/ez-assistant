import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { ConversationItem } from "@ez-assistant/protocol";
import { McpControlResult } from "../../src/features/conversation/McpControlResult";
import { groupConversationTurns } from "../../src/features/conversation/ConversationView/conversationRows";

afterEach(cleanup);

it("describes reselecting the same Shell as an explicit environment refresh", () => {
  render(<McpControlResult message={{ type: "shell_switch_result", message_id: "refresh", shell: "cmd", previous_shell: "cmd", success: true }} />);
  expect(screen.getByRole("article", { name: "已刷新 Agent Shell：CMD" })).toHaveAttribute("data-outcome", "success");
});

it("keeps a successful shell switch separate from user and assistant messages", () => {
  const message: Extract<ConversationItem, { type: "shell_switch_result" }> = {
    type: "shell_switch_result", message_id: "switch", run_id: "run", shell: "powershell_7", previous_shell: "cmd", success: true,
  };
  expect(groupConversationTurns([message])).toEqual([{ type: "control_result", message }]);
  render(<McpControlResult message={message} />);
  expect(screen.getByRole("article", { name: "已切换 Agent Shell：CMD → PowerShell 7" })).toHaveAttribute("data-outcome", "success");
  expect(screen.getByText("从这条指令开始，后续命令使用 PowerShell 7 语法。")).toBeVisible();
});

it("shows an inherited failed switch without a local Run", () => {
  render(<McpControlResult message={{
    type: "shell_switch_result", message_id: "failed", shell: "git_bash", previous_shell: "powershell_7", success: false,
  }} />);
  expect(screen.getByRole("article", { name: "切换到 Git Bash 失败" })).toHaveAttribute("data-outcome", "failure");
  expect(screen.getByText("此次切换未生效，已保留PowerShell 7。")).toBeInTheDocument();
});
