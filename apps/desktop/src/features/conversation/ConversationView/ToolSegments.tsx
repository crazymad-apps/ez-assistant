import { Fragment, useId, useState } from "react";
import type {
  AssistantSegment,
  ChildTaskTreeItemSnapshot,
  MessageId,
  McpToolIdentity,
  ToolActivityStatus,
  ToolCallId,
  ToolEventSnapshot,
  ToolInputSnapshot,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import { ConversationMarkdownContent } from "../../resource-workspace/ConversationMarkdownContent";
import type {
  LiveExecutionSegment,
  LiveRunProjection,
  LiveToolSnapshot,
} from "../../../stores/LiveExecutionStore";
import { ChildTaskTree } from "./ChildTaskTree";
import {
  humanizeToolName,
  liveSegmentStateKey,
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
    return <ReasoningBox is_open={props.is_reasoning_open} on_toggle={props.on_reasoning_toggle} text={props.segment.text} />;
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

export function LiveSteps(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  onToolClick: (tool: LiveToolSnapshot) => void;
  run: LiveRunProjection;
}>) {
  const [reasoning_open, setReasoningOpen] = useState<Record<string, boolean>>({});
  if (props.run.steps.every((step) => step.segments.length === 0)) {
    return props.run.status === "accepted" || props.run.status === "running"
      ? <p className={styles.waiting_text}>正在准备…</p>
      : null;
  }
  return props.run.steps.map((step) => (
    <div className={styles.assistant_step} key={`${props.run.run_id}:step:${step.step}`}>
      {step.segments.map((segment, index) => {
        const state_key = liveSegmentStateKey(props.run.run_id, step.step, segment, index);
        return <LiveSegmentView
          child_tasks={props.child_tasks}
          key={state_key}
          is_reasoning_open={segment.type === "reasoning" ? (reasoning_open[state_key] ?? true) : false}
          on_reasoning_toggle={() => segment.type === "reasoning" && setReasoningOpen((current) => ({ ...current, [state_key]: !(current[state_key] ?? true) }))}
          on_child_open={props.on_child_open}
          on_tool_click={props.onToolClick}
          segment={segment}
        />;
      })}
    </div>
  ));
}

function LiveSegmentView(props: Readonly<{
  child_tasks: readonly ChildTaskTreeItemSnapshot[];
  segment: LiveExecutionSegment;
  is_reasoning_open: boolean;
  on_child_open?: (item: ChildTaskTreeItemSnapshot) => void;
  on_reasoning_toggle: () => void;
  on_tool_click: (tool: LiveToolSnapshot) => void;
}>) {
  if (props.segment.type === "reasoning") {
    return <ReasoningBox is_open={props.is_reasoning_open} on_toggle={props.on_reasoning_toggle} text={props.segment.text} />;
  }
  if (props.segment.type === "text") {
    return <div className={styles.assistant_text}><ConversationMarkdownContent is_streaming text={props.segment.text} /></div>;
  }
  return <LiveToolGroup child_tasks={props.child_tasks} on_child_open={props.on_child_open} on_tool_click={props.on_tool_click} tools={props.segment.tools} />;
}

function ReasoningBox(props: Readonly<{ text: string; is_open: boolean; on_toggle: () => void }>) {
  const content_id = useId();
  return (
    <section className={styles.reasoning} data-open={props.is_open}>
      <button
        aria-controls={content_id}
        aria-expanded={props.is_open}
        className={styles.reasoning_header}
        onClick={props.on_toggle}
        type="button"
      >
        <span>思考过程</span>
        <Icon name="chevron-down" size={15} />
      </button>
      <Collapse class_name={styles.reasoning_content} id={content_id} open={props.is_open}>{props.text}</Collapse>
    </section>
  );
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
        <ToolRow identity={tool.mcp_identity} name={tool.tool_name} on_click={() => props.on_tool_click(tool)} status={tool.status} summary={toolSummary(tool)} />
        {props.on_child_open && <ChildTaskTree embedded items={tool_child_tasks} on_open={props.on_child_open} />}
      </Fragment>
    );
  })}</div>;
}

function ToolRow(props: Readonly<{
  name: string;
  status: ToolActivityStatus;
  summary?: string | null;
  input?: ToolInputSnapshot;
  identity?: McpToolIdentity;
  on_click?: () => void;
}>) {
  const visible_summary = visibleToolSummary(props.summary);
  const input_label = props.input ? toolInputLabel(props.input) : null;
  const skill_name = props.name === "load_skill" ? loadSkillName(props.input) : null;
  const identity = props.identity ?? (props.input?.type === "mcp" ? props.input.identity : null);
  const label = identity ? `${identity.server_display_name} (${identity.server_key}) / ${identity.tool_name}` : humanizeToolName(props.name);
  const content = <>
    <strong data-status={props.status}>{label}{skill_name ? ` · ${skill_name}` : ""}</strong>
    <span className={styles.tool_status} data-status={props.status}>{toolStatusLabel(props.status)}</span>
    {input_label && !identity && !skill_name && props.name !== "load_skill" && <span className={styles.tool_input}>{input_label}</span>}
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
