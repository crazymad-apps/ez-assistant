import { useCallback, useId, useState, type FormEvent } from "react";
import type {
  ApprovalMode, CustomAgentSelection, ScheduleRule, ScheduleSettings, WorkspaceSummary,
} from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import { ModelPicker, modelSelectionLabel } from "../../settings/ModelPicker";
import type { RuntimeClient } from "../../../runtime-client/RuntimeClient";
import { deviceTime, schedule_frequencies, weekdays } from "./display";
import { ScheduleDateField } from "./ScheduleRuleFields/DateField";
import { ScheduleTimeField, isScheduleTime } from "./ScheduleRuleFields/TimeField";
import { useScheduleRead } from "./useScheduleRead";
import styles from "./index.module.scss";

const default_settings: ScheduleSettings = {
  name: "", enabled: true, rule: { type: "daily", time: "09:00" },
  action: { type: "agent", instructions: "", approval_mode: "ask" }, report_to_controller: true,
};

export function ScheduleForm(props: Readonly<{
  client: RuntimeClient;
  workspaces: readonly WorkspaceSummary[];
  workspace_error: string | null;
  on_retry_workspaces: () => void;
  initial?: ScheduleSettings;
  initial_workspace?: string;
  busy: boolean;
  on_cancel: () => void;
  on_save: (settings: ScheduleSettings) => void;
}>) {
  const { client } = props;
  const [settings, setSettings] = useState(props.initial ?? { ...default_settings, workspace_id: props.initial_workspace });
  const [model_open, setModelOpen] = useState(false);
  const [option_revision, setOptionRevision] = useState(0);
  const form_id = useId();
  const { rule, action } = settings;
  const agent = action.type === "agent";
  const read_preview = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "preview_schedule", payload: { rule } }, { signal })).payload,
  [client, rule]);
  const read_agents = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "list_custom_agents", payload: {} }, { signal })).payload,
  [client]);
  const read_models = useCallback(async (signal: AbortSignal) => {
    const [providers, model_settings] = await Promise.all([
      client.command({ type: "list_providers", payload: {} }, { signal }),
      client.command({ type: "get_model_settings", payload: {} }, { signal }),
    ]);
    return { providers: providers.payload, model_settings: model_settings.payload };
  }, [client]);
  const preview_ready = isScheduleTime(rule.time) && (rule.type !== "once" || !!rule.date)
    && (rule.type !== "weekly" || rule.weekdays.length > 0);
  const preview = useScheduleRead(preview_ready ? read_preview : null, 0, 0);
  const agents = useScheduleRead(agent ? read_agents : null, option_revision, 0);
  const models = useScheduleRead(agent ? read_models : null, option_revision, 0);
  const selection = agent ? action.model_selection : undefined;
  const setRule = (next: ScheduleRule) => setSettings((previous) => ({ ...previous, rule: next }));

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (props.busy || !preview_ready || !preview.value || preview.error) return;
    props.on_save({ ...settings, name: settings.name.trim() });
  }

  const agent_options = new Map<string, { selection: CustomAgentSelection; label: string; description: string }>((agents.value?.agents ?? []).map((item) => {
    const selection: CustomAgentSelection = { name: item.config.name, source: item.source };
    return [agentKey(selection), { selection, label: item.config.name, description: item.source.type === "personal" ? "个人" : "企业" }] as const;
  }));
  if (agent && action.custom_agent && !agent_options.has(agentKey(action.custom_agent))) {
    agent_options.set(agentKey(action.custom_agent), { selection: action.custom_agent, label: action.custom_agent.name, description: "当前配置" });
  }
  const workspace_options = [{ value: "", label: "不关联项目" },
    ...props.workspaces.map((workspace) => ({ value: workspace.workspace_id, label: workspace.label, description: workspace.user_directory ?? undefined }))];
  if (settings.workspace_id && !workspace_options.some((workspace) => workspace.value === settings.workspace_id)) {
    workspace_options.push({ value: settings.workspace_id, label: "已关联项目（当前不可用）" });
  }
  const options_error = agents.error ?? models.error ?? props.workspace_error;
  return <form className={styles.form} onSubmit={submit}>
    <fieldset disabled={props.busy}>
      <label className={styles.field}>名称
        <input name="schedule-name" maxLength={80} required value={settings.name}
          onChange={(event) => setSettings({ ...settings, name: event.target.value })} />
      </label>

      <div className={styles.fields_row}>
        <div className={styles.field}>
          <span>任务类型</span>
          <Choice label="任务类型" selected={action.type} options={[
            { value: "agent", label: "Agent 任务" }, { value: "script", label: "脚本任务" },
          ]} on_select={(type) => {
            if (type === action.type) return;
            setModelOpen(false);
            setSettings({ ...settings, action: type === "script"
              ? { type: "script", command: "" }
              : { type: "agent", instructions: "", approval_mode: "ask" } });
          }} />
        </div>
        <div className={styles.field}>
          <span>关联项目</span>
          <Choice label="关联项目" selected={settings.workspace_id ?? ""} options={workspace_options}
            on_select={(workspace_id) => setSettings({ ...settings, workspace_id: workspace_id || undefined })} />
        </div>
      </div>

      <div className={styles.schedule_fields}>
        <div className={styles.fields_row}>
          <div className={styles.field}>
            <span>执行频率</span>
            <Choice label="执行频率" selected={rule.type} options={[
              { value: "once", label: schedule_frequencies.once }, { value: "daily", label: schedule_frequencies.daily },
              { value: "weekly", label: schedule_frequencies.weekly },
            ]} on_select={(type: ScheduleRule["type"]) => {
              if (type === rule.type) return;
              if (type === "once") setRule({ type, time: rule.time, date: "" });
              if (type === "daily") setRule({ type, time: rule.time });
              if (type === "weekly") setRule({ type, time: rule.time, weekdays: [] });
            }} />
          </div>
          {rule.type === "once" && <ScheduleDateField value={rule.date} disabled={props.busy}
            on_change={(date) => setRule({ ...rule, date })} />}
          <ScheduleTimeField value={rule.time} disabled={props.busy}
            on_change={(time) => setRule({ ...rule, time })} />
        </div>
        {rule.type === "weekly" && <div className={styles.weekdays} role="group" aria-label="执行星期">
          {weekdays.map(([day, label]) => <label key={day}>
            <input type="checkbox" checked={rule.weekdays.includes(day)} onChange={(event) => {
              const selected = event.target.checked;
              setRule({ ...rule, weekdays: selected ? [...rule.weekdays, day] : rule.weekdays.filter((value) => value !== day) });
            }} /> {label}
          </label>)}
        </div>}
        {preview.error ? <p role="alert">{preview.error}</p> : <output className={styles.preview}>
          {preview.value?.next_run_at ? `${settings.enabled ? "下次触发" : "启用后下次触发"} ${deviceTime(preview.value.next_run_at)}`
            : preview.value ? "此规则没有未来触发时间"
            : preview_ready ? "正在计算下次触发…" : "填写执行时间后显示下次触发"}
        </output>}
      </div>

      {action.type === "script"
        ? <label className={styles.field}>Shell 脚本
          <textarea className={styles.code_input} required name="schedule-script" rows={7} value={action.command}
            onChange={(event) => setSettings({ ...settings, action: { type: "script", command: event.target.value } })} />
        </label>
        : <>
          <label className={styles.field}>任务指令
            <textarea required rows={6} value={action.instructions}
              onChange={(event) => setSettings({ ...settings, action: { ...action, instructions: event.target.value } })} />
          </label>
          <div className={styles.fields_row}>
            <div className={styles.field}>
              <span>智能体</span>
              <Choice label="智能体" selected={action.custom_agent ? agentKey(action.custom_agent) : ""}
                options={[{ value: "", label: "默认智能体" },
                  ...Array.from(agent_options, ([value, entry]) => ({ value, label: entry.label, description: entry.description }))]}
                on_select={(value) => setSettings({ ...settings, action: { ...action, custom_agent: agent_options.get(value)?.selection } })} />
            </div>
            <div className={styles.field}>
              <span>模型</span>
              <ModelPicker title="模型" label={selection ? modelSelectionLabel(selection, models.value?.providers ?? [], false) : "默认模型"}
                selection={selection ?? null} providers={models.value?.providers ?? []}
                runtime_target={{ client, model_settings: models.value?.model_settings }}
                trigger_variant="field" open={model_open} onOpenChange={setModelOpen}
                disabled={props.busy} show_manage_providers={false} clearable
                onSelect={async (next) => {
                  setSettings({ ...settings, action: { ...action, model_selection: next ?? undefined } });
                  return true;
                }} />
            </div>
            <div className={styles.field}>
              <span>工具权限模式</span>
              <Choice label="工具权限模式" selected={action.approval_mode} options={[
                { value: "auto", label: "按规则自动决定" }, { value: "ask", label: "需要授权时询问" },
              ]} on_select={(approval_mode: ApprovalMode) => setSettings({ ...settings, action: { ...action, approval_mode } })} />
            </div>
          </div>
          <p className={styles.hint}>定时执行时，任何需要人工审批的工具调用都会失败。</p>
        </>}
      {options_error && <p role="alert">{options_error} <Button variant="text" onClick={() => { setOptionRevision((value) => value + 1); props.on_retry_workspaces(); }}>重试读取选项</Button></p>}
      <label htmlFor={form_id} className={styles.checkbox}>
        <input id={form_id} type="checkbox" checked={settings.report_to_controller}
          onChange={(event) => setSettings({ ...settings, report_to_controller: event.target.checked })} />
        将执行结果返回主控会话
      </label>
    </fieldset>
    <footer className={styles.actions}>
      <Button disabled={props.busy} onClick={props.on_cancel}>取消</Button>
      <Button variant="primary" type="submit" disabled={props.busy || !preview_ready || !preview.value || !!preview.error || !settings.name.trim()}>
        {props.busy ? "正在保存…" : "保存计划"}
      </Button>
    </footer>
  </form>;
}

function agentKey(selection: CustomAgentSelection): string {
  const source = selection.source;
  return source.type === "personal" ? JSON.stringify(["personal", selection.name]) :
    JSON.stringify(["center", source.center_id, source.organization_id ?? null, selection.name]);
}

function Choice<T extends string>(props: Readonly<{
  label: string; options: readonly SelectionOption<T>[]; selected: T; on_select: (value: T) => void;
}>) {
  const [open, setOpen] = useState(false);
  return <SelectionPopover aria_label={props.label} trigger_variant="field" content_width="trigger" selected={props.selected}
    options={props.options} open={open} on_open_change={setOpen} on_select={props.on_select} />;
}
