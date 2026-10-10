import { Fragment } from "react";
import { Icon } from "../../../components/Icon";
import { toolPresentation } from "./toolPresentation";
import type {
  AssistantSegment,
  ChildTaskTreeItemSnapshot,
  MessageId,
  McpToolIdentity,
  ToolActivityStatus,
  ToolCallId,
  ToolEventSnapshot,
  ToolInputSnapshot,
  ToolInputProjection,
} from "@ez-assistant/protocol";
import { ReasoningBox } from "./ReasoningBox";
import { ConversationMarkdownContent } from "../../resource-workspace/ConversationMarkdownContent";
import type {
  LiveExecutionSegment,
  LiveToolSnapshot,
} from "../../../stores/LiveExecutionStore";
import { ChildTaskTree } from "./ChildTaskTree";
import {
  humanizeToolName,
  toolInputLabel,
  toolStatusLabel,
  toolSummary,
  visibleToolSummary,
} from "./conversationRows";
import styles from "./index.module.scss";

export function AssistantSegmentView(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  segment: AssistantSegment;
  message_id: MessageId;
  is_reasoning_open: boolean;
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  on_reasoning_toggle: () => void;
  on_tool_click: (message_id: MessageId, call_id: ToolCallId) => void;
}>) {
  if (props.segment.type === "reasoning") {
    return <ReasoningBox summary={props.segment.source === "summary"} state_key={`${props.message_id}:${props.segment.part_id}`} is_open={props.is_reasoning_open} on_toggle={props.on_reasoning_toggle} text={props.segment.text} />;
  }
  if (props.segment.type === "text") {
    return (
      <div
        className={styles.assistant_text}
        data-quote-content="true"
      >
        <ConversationMarkdownContent text={props.segment.text} />
      </div>
    );
  }
  return (
    <ToolGroup
      child_tasks={props.child_tasks}
      on_child_open={props.on_child_open}
      on_tool_click={(call_id) => props.on_tool_click(props.message_id, call_id)}
      tools={props.segment.tools}
    />
  );
}

export function LiveSegmentView(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  state_key: string;
  segment: LiveExecutionSegment;
  is_reasoning_open: boolean;
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  on_reasoning_toggle: () => void;
  on_tool_click: (tool: LiveToolSnapshot) => void;
}>) {
  if (props.segment.type === "reasoning") {
    return <ReasoningBox summary={props.segment.source === "summary"} state_key={props.state_key} is_open={props.is_reasoning_open} on_toggle={props.on_reasoning_toggle} text={props.segment.text} />;
  }
  if (props.segment.type === "text") {
    return <div className={styles.assistant_text}><ConversationMarkdownContent is_streaming text={props.segment.text} /></div>;
  }
  return <LiveToolGroup child_tasks={props.child_tasks} on_child_open={props.on_child_open} on_tool_click={props.on_tool_click} tools={props.segment.tools} />;
}

function ToolGroup(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  tools: readonly ToolEventSnapshot[];
  on_tool_click: (call_id: ToolCallId) => void;
}>) {
  return <div className={styles.tool_group}>{props.tools.map((tool) => {
    const tool_child_tasks = props.child_tasks.filter((item) => item.task.parent_tool_call_id === tool.call_id);
    return (
      <Fragment key={tool.call_id}>
        <ToolRow identity={tool.mcp_identity} input={tool.input} name={tool.tool_name} on_click={() => props.on_tool_click(tool.call_id)} status={tool.status} summary={tool.summary} />
        {props.on_child_open && <ChildTaskTree embedded items={tool_child_tasks} on_open={props.on_child_open} />}
      </Fragment>
    );
  })}</div>;
}

function LiveToolGroup(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  on_tool_click: (tool: LiveToolSnapshot) => void;
  tools: readonly LiveToolSnapshot[];
}>) {
  return <div className={styles.tool_group}>{props.tools.map((tool) => {
    const tool_child_tasks = props.child_tasks.filter((item) => item.task.parent_tool_call_id === tool.call_id);
    return (
      <Fragment key={tool.call_id}>
        <ToolRow identity={tool.mcp_identity} input={tool.input} name={tool.tool_name} on_click={() => props.on_tool_click(tool)} status={tool.status} summary={toolSummary(tool)} />
        {props.on_child_open && <ChildTaskTree embedded items={tool_child_tasks} on_open={props.on_child_open} />}
      </Fragment>
    );
  })}</div>;
}

function ToolRow(props: Readonly<{
  name: string;
  status: ToolActivityStatus;
  summary?: string | null;
  input?: ToolInputProjection;
  identity?: McpToolIdentity;
  on_click?: () => void;
}>) {
  const visible_summary = visibleToolSummary(props.summary);
  const input = props.input?.value;
  const input_label = input ? toolInputLabel(input) : null;
  const skill_name = props.name === "load_skill" ? loadSkillName(input) : null;
  const identity = props.identity ?? (input?.type === "mcp" ? input.identity : null);
  const label = identity ? `${identity.server_display_name} (${identity.server_key}) / ${identity.tool_name}` : humanizeToolName(props.name);
  const content = <>
    <Icon name={identity ? "puzzle" : toolPresentation(props.name).icon} size={16} />
    <strong data-status={props.status}>{label}{skill_name ? ` · ${skill_name}` : ""}</strong>
    <span className={styles.tool_status} data-status={props.status}>{toolStatusLabel(props.status)}</span>
    {input_label && !identity && !skill_name && props.name !== "load_skill" && <span className={styles.tool_input}>{input_label}</span>}
    {props.input?.redacted && <span className={styles.tool_summary}>· 已隐去敏感值</span>}
    {props.input?.truncated && <span className={styles.tool_summary}>· 内容已截断</span>}
    {visible_summary && <span className={styles.tool_summary}>· {visible_summary}</span>}
  </>;
  return props.on_click
    ? <button className={styles.tool_row} onClick={props.on_click} type="button">{content}</button>
    : <div className={styles.tool_row}>{content}</div>;
}

function loadSkillName(input: ToolInputSnapshot | undefined): string | null {
  if (input?.type !== "general") return null;
  try {
    const value = JSON.parse(input.summary) as { name?: unknown };
    return typeof value.name === "string" ? value.name : null;
  } catch {
    return null;
  }
}
