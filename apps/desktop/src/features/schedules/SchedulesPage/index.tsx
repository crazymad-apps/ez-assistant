import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import type {
  ContextPage, ScheduleDelivery, ScheduleExecution, ScheduleResultFile, ScheduleSettings, ScheduleSnapshot,
} from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { Collapse } from "../../../components/Collapse";
import { Dialog } from "../../../components/Dialog";
import { Icon } from "../../../components/Icon";
import { InlineIconButton } from "../../../components/InlineIconButton";
import type { ClientResources } from "../../../runtime-client/ClientResources";
import { isCommandOutcomeUnknown, RuntimeClientError, type RuntimeClient } from "../../../runtime-client/RuntimeClient";
import { useRootStore } from "../../../stores/RootStoreContext";
import { DevicePicker } from "../../runtime-access/DevicePicker";
import { errorText, executionStatus, planStatus, ruleLabel, scheduledTime } from "./display";
import { ExecutionResult } from "./ExecutionResult";
import { PlanDefinition } from "./PlanDefinition";
import { ScheduleForm } from "./ScheduleForm";
import { useScheduleRead } from "./useScheduleRead";
import styles from "./index.module.scss";

const PAGE_SIZE = 10;

export const SchedulesPage = observer(function SchedulesPage(props: Readonly<{
  on_close: () => void; sidebar_open: boolean; on_toggle_sidebar: () => void;
}>) {
  const store = useRootStore();
  const device = store.network.browsing_device;
  const context = store.sidebar_context;
  const client = device ? context?.runtime.client : store.local_runtime_client;
  const connection = store.sidebar_connection;
  const files = device ? context?.files : store.local_files;
  const observed = useRef({ device, client });
  observed.current = { device, client };
  const connected = connection.state === "connected";
  const navigation = {
    sidebar_open: props.sidebar_open,
    on_toggle_sidebar: props.on_toggle_sidebar,
    on_back: props.on_close,
    device_control: store.network.available ? <DevicePicker aria_label="定时任务设备" device={device}
      on_select={(target) => store.network.select(target)} /> : <span className={styles.device}>{store.network.label(device)}</span>,
  };
  const connection_notice = !connected && <div className={styles.connection} role="status">
      <span>{store.network.label(device)} · {connection.error_message ?? "正在连接 Runtime…"}</span>
      <Button variant="text" onClick={() => device ? context?.runtime.retryConnection() : store.retryLocalConnection()}>重新连接</Button>
    </div>;
  return <section className={styles.page} aria-label="定时任务">
    {client && files ? <ScheduleWorkspace
      key={`${device ?? "local"}:${client.ui_client_instance_id}`} client={client} files={files} connected={connected}
      navigation={navigation} connection_notice={connection_notice}
      initial_workspace={store.navigation.target_device === device
        ? store.projection.sessionSummary(store.navigation.selected_session_id)?.workspace_id
          ?? store.new_session_drafts.get(store.navigation.selected_draft_key)?.workspace_id ?? undefined
        : undefined}
      on_delivery={async (delivery) => {
        if (observed.current.device !== device || observed.current.client !== client) return;
        await store.selectSession(delivery.session_id, device);
        if (observed.current.device !== device || observed.current.client !== client) return;
        if (!await store.locateConversationMessage(delivery.session_id, delivery.message_id)) {
          throw new Error("无法定位这条结果汇报，请刷新后重试。");
        }
        if (observed.current.device === device && observed.current.client === client) props.on_close();
      }} /> : <><ScheduleNavigation {...navigation} />{connection_notice}</>}
  </section>;
});

type ScheduleNavigationProps = Readonly<{
  sidebar_open: boolean;
  on_toggle_sidebar: () => void;
  on_back: () => void;
  device_control: ReactNode;
  back_label?: string;
  back_disabled?: boolean;
}>;

function ScheduleNavigation(props: ScheduleNavigationProps) {
  const back_label = props.back_label ?? "返回会话";
  return <nav className={styles.topbar} aria-label="定时任务导航" data-tauri-drag-region="deep">
    {!props.sidebar_open && <InlineIconButton icon="sidebar-left" label="展开侧栏" onClick={props.on_toggle_sidebar} />}
    <InlineIconButton icon="chevron-left" label={back_label} title={back_label} disabled={props.back_disabled} onClick={props.on_back} />
    <strong>定时任务</strong>
    {props.device_control}
  </nav>;
}

function ScheduleWorkspace(props: Readonly<{
  client: RuntimeClient; files: ClientResources; connected: boolean; initial_workspace?: string;
  navigation: ScheduleNavigationProps; connection_notice: ReactNode;
  on_delivery: (delivery: ScheduleDelivery) => Promise<void>;
}>) {
  const { client, connected } = props;
  const [schedule_id, setScheduleId] = useState<string | null>(null);
  const [execution_id, setExecutionId] = useState<string | null>(null);
  const [editor, setEditor] = useState<"create" | ScheduleSnapshot | null>(null);
  const [offset, setOffset] = useState(0);
  const [history_offset, setHistoryOffset] = useState(0);
  const [revision, setRevision] = useState(0);
  const [busy, setBusy] = useState<string | null>(null);
  const lock = useRef(false);
  const [message, setMessage] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [delete_open, setDeleteOpen] = useState(false);
  const [reconcile, setReconcile] = useState<{ kind: string; schedule_id: string | null } | null>(null);
  const manual_key = useRef<{ schedule_id: string; key: string } | null>(null);
  const focus_execution = useRef<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const title = useRef<HTMLHeadingElement>(null);
  const execution_panel_prefix = useId();
  const refresh = useCallback(() => setRevision((value) => value + 1), []);
  const read_list = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "list_schedules", payload: { page: { offset, limit: PAGE_SIZE } } }, { signal })).payload, [client, offset]);
  const read_plan = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "get_schedule", payload: { schedule_id: schedule_id! } }, { signal })).payload, [client, schedule_id]);
  const read_history = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "list_schedule_executions", payload: {
      schedule_id: schedule_id!, page: { offset: history_offset, limit: PAGE_SIZE },
    } }, { signal })).payload, [client, schedule_id, history_offset]);
  const read_execution = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "get_schedule_execution", payload: { execution_id: execution_id! } }, { signal })).payload, [client, execution_id]);
  const read_workspaces = useCallback(async (signal: AbortSignal) =>
    (await client.command({ type: "list_workspaces", payload: { lifecycle: "active" } }, { signal })).payload.workspaces, [client]);
  const list = useScheduleRead(!schedule_id && !editor ? read_list : null, revision, 2000, connected);
  const plan = useScheduleRead(schedule_id ? read_plan : null, revision, 2000, connected);
  const history = useScheduleRead(schedule_id && !editor ? read_history : null, revision, 2000, connected);
  const current = useScheduleRead(execution_id && !editor ? read_execution : null, revision, 1000, connected);
  const workspaces = useScheduleRead(editor || schedule_id ? read_workspaces : null, revision, 0, connected);
  const active_plan = plan.value;
  const controls_disabled = !!busy || !!reconcile || !connected;

  const is_editing = editor !== null;
  useEffect(() => {
    if (scroller.current) scroller.current.scrollTop = 0;
    title.current?.focus({ preventScroll: true });
  }, [schedule_id, is_editing]);
  useEffect(() => {
    if (list.value && offset > 0 && list.value.page.offset >= list.value.page.total) {
      setOffset(Math.max(0, Math.floor((list.value.page.total - 1) / PAGE_SIZE) * PAGE_SIZE));
    }
  }, [list.value, offset]);

  function selectPlan(id: string | null) {
    focus_execution.current = null;
    setScheduleId(id);
    setExecutionId(null);
    setHistoryOffset(0);
    setEditor(null);
    setDeleteOpen(false);
    setNotice(null);
    setMessage(null);
  }

  function cancelEditor() {
    setEditor(null);
    setMessage(null);
    setNotice(null);
  }

  async function verify(target: NonNullable<typeof reconcile>) {
    if (target.schedule_id === null) {
      await read_list(new AbortController().signal);
      selectPlan(null);
      setOffset(0);
      setNotice("请在列表核对刚才提交的计划。");
      return;
    }
    try {
      await client.command({ type: "get_schedule", payload: { schedule_id: target.schedule_id } });
    } catch (error) {
      if (error instanceof RuntimeClientError && error.code === "schedule_not_found") {
        selectPlan(null);
        return;
      }
      throw error;
    }
    if (target.kind === "run") {
      await client.command({ type: "list_schedule_executions", payload: {
        schedule_id: target.schedule_id, page: { offset: 0, limit: PAGE_SIZE },
      } });
      setHistoryOffset(0);
    }
  }

  async function mutate(kind: string, operation: () => Promise<void>, verify_write = true) {
    if (lock.current || !connected) return;
    lock.current = true;
    setBusy(kind);
    setMessage(null);
    setNotice(null);
    try {
      await operation();
      setReconcile(null);
    } catch (error) {
      const unknown = verify_write && isCommandOutcomeUnknown(error);
      setMessage(unknown ? `请求结果未知。正在回读核对。 ${errorText(error)}` : errorText(error));
      if (unknown) {
        const target = { kind, schedule_id };
        setReconcile(target);
        try {
          await verify(target);
          setReconcile(null);
          setMessage(null);
          setNotice(target.schedule_id ? "已刷新，请核对最新状态。" : "已刷新，请在列表核对刚才提交的计划。");
        } catch (read_error) {
          setMessage(`请求结果未知，暂时无法核对。 ${errorText(read_error)}`);
        }
      }
    } finally {
      refresh();
      setBusy(null);
      lock.current = false;
    }
  }

  function submit(settings: ScheduleSettings) {
    void mutate("save", async () => {
      const plan = typeof editor === "object" && editor
        ? await client.command({ type: "update_schedule", payload: { schedule_id: editor.schedule_id, settings, expected_settings: editor.settings } })
        : await client.command({ type: "create_schedule", payload: settings });
      selectPlan(plan.payload.schedule_id);
    });
  }

  function runNow(id: string) {
    const key = manual_key.current?.schedule_id === id ? manual_key.current.key : crypto.randomUUID();
    manual_key.current = { schedule_id: id, key };
    void mutate("run", async () => {
      const { payload: result } = await client.command({
        type: "run_schedule_now", payload: { schedule_id: id, idempotency_key: key },
      });
      manual_key.current = null;
      focus_execution.current = result.execution.execution_id;
      setHistoryOffset(0);
      setExecutionId(result.execution.execution_id);
      if (result.type === "busy") setNotice("已有执行占用名额，正在查看这次执行。");
      if (result.type === "replayed") setNotice("已找到此前接纳的执行。");
    });
  }

  function remove(id: string) {
    void mutate("delete", async () => {
      try {
        await client.command({ type: "delete_schedule", payload: { schedule_id: id } });
      } catch (error) {
        if (!(error instanceof RuntimeClientError && error.code === "schedule_not_found")) throw error;
      }
      selectPlan(null);
    });
  }

  function executionAction(execution: ScheduleExecution, kind: "stop_schedule_execution" | "retry_schedule_result") {
    void mutate(kind, async () => {
      const payload = { execution_id: execution.execution_id };
      if (kind === "stop_schedule_execution") {
        await client.command({ type: "stop_schedule_execution", payload });
        setNotice("已提交停止请求，正在等待执行收尾。");
      } else await client.command({ type: "retry_schedule_result", payload });
    });
  }

  function renderExecution(execution: ScheduleExecution) {
    const { execution_id } = execution;
    return <ExecutionResult key={execution_id} client={client} execution={execution}
      stale={!connected || !!current.error} busy={controls_disabled} on_refresh={refresh}
      on_stop={() => executionAction(execution, "stop_schedule_execution")}
      on_retry={() => executionAction(execution, "retry_schedule_result")}
      on_stop_background={(background_id) => void mutate("stop_background", async () => {
        const result = await client.command({ type: "stop_schedule_background", payload: { execution_id, background_id } });
        if (result.payload.output_error) setMessage(result.payload.output_error);
      })}
      on_download={(file: ScheduleResultFile) => void mutate("download", () =>
        props.files.downloadScheduleResultFile(execution_id, file.file_id, file.name), false)}
      on_bundle_download={() => void mutate("download", () => props.files.downloadScheduleResultBundle(
        execution_id, execution.files.map((file) => file.file_id),
      ), false)}
      on_delivery={(delivery) => void mutate("delivery", () => props.on_delivery(delivery), false)} />;
  }

  const executions = history.value?.executions ?? [];
  const selected_in_history = executions.some((item) => item.execution_id === execution_id);
  // 当立即执行先于分页回读返回，或已有活动记录排在分页之外时，仍可在对应行查看它。
  const visible_executions = current.value && !selected_in_history
    ? [...executions, current.value].sort((left, right) => right.accepted_at_ms - left.accepted_at_ms)
    : executions;
  const read_error = list.error ?? plan.error ?? history.error ?? (selected_in_history ? null : current.error);
  const label = editor === "create" ? "新建定时任务" : editor ? "编辑定时任务"
    : schedule_id ? active_plan?.settings.name ?? "计划详情" : "任务计划";
  const back_label = editor && schedule_id ? "返回计划详情" : schedule_id || editor ? "返回计划列表" : undefined;
  const go_back = editor ? cancelEditor : schedule_id ? () => selectPlan(null) : props.navigation.on_back;

  return <>
    <ScheduleNavigation {...props.navigation} back_label={back_label} on_back={go_back}
      back_disabled={!!back_label && (!!busy || !!reconcile)} />
    {props.connection_notice}
    <div className={styles.scroller} ref={scroller}>
      <div className={styles.workspace}>
        <header className={styles.heading}>
          <div className={styles.title}>
            <h1 ref={title} tabIndex={-1}>{label}</h1>
          </div>
          {!schedule_id && !editor && <Button variant="primary" disabled={controls_disabled} onClick={() => {
            setMessage(null); setNotice(null); setEditor("create");
          }}><Icon name="plus" size={16} /> 新建计划</Button>}
        </header>
        {message && !delete_open && <p className={styles.error} role="alert">{message}</p>}
        {notice && <p className={styles.notice} role="status">{notice}</p>}
        {reconcile && !delete_open && <Button disabled={!!busy || !connected} onClick={() =>
          void mutate("verify", () => verify(reconcile), false)}>刷新并核对</Button>}
        {read_error && !editor && <div className={styles.error} role="alert">
          {plan.missing ? "此计划已删除或不可用。" : current.missing ? "此执行记录已不可用。" : read_error}
          <Button size="small" variant="text" onClick={refresh}>刷新</Button>
        </div>}
        {editor && <ScheduleForm client={client} initial={editor === "create" ? undefined : editor.settings}
          initial_workspace={editor === "create" ? props.initial_workspace : undefined}
          busy={controls_disabled} workspaces={workspaces.value ?? []} workspace_error={workspaces.error}
          on_retry_workspaces={refresh} on_cancel={cancelEditor}
          on_save={submit} />}
        {!editor && !schedule_id && list.value && <>
          {list.value.schedules.length === 0 ? <p className={styles.empty}>暂无定时任务</p> : <ul className={styles.plans}>
            {list.value.schedules.map((plan) => <li key={plan.schedule_id}>
              <button type="button" className={styles.plan_row} onClick={() => selectPlan(plan.schedule_id)}>
                <span className={styles.plan_icon}><Icon name={plan.settings.action.type === "script" ? "terminal" : "bot"} size={19} /></span>
                <span className={styles.plan_info}><strong>{plan.settings.name}</strong>
                  <small>{plan.settings.action.type === "script" ? "脚本" : "Agent"} · {ruleLabel(plan.settings.rule)}</small></span>
                <span className={styles.plan_date}><small>下次触发</small><span>{scheduledTime(plan)}</span></span>
                <span className={styles.plan_status} data-enabled={planStatus(plan) === "已启用"}>{planStatus(plan)}</span>
                <Icon name="chevron-right" size={15} />
              </button>
            </li>)}
          </ul>}
          <Pagination label="任务计划" page={list.value.page} on_change={setOffset} />
        </>}
        {!editor && !schedule_id && !list.value && !read_error && connected && <p role="status">正在读取计划…</p>}
        {!editor && active_plan && <>
          <div className={styles.actions}>
            <Button variant="primary" disabled={controls_disabled || !!plan.error || active_plan.deleting} onClick={() => runNow(active_plan.schedule_id)}>立即执行</Button>
            <Button disabled={controls_disabled || !!plan.error || active_plan.deleting} onClick={() =>
              void mutate("enable", async () => {
                await client.command({ type: "set_schedule_enabled", payload: {
                  schedule_id: active_plan.schedule_id, enabled: !active_plan.settings.enabled,
                } });
              })}>{active_plan.settings.enabled ? "暂停" : "启用"}</Button>
            <Button disabled={controls_disabled || !!plan.error || active_plan.deleting} onClick={() => { setMessage(null); setEditor(active_plan); }}>编辑</Button>
            <Button variant="text" className={styles.destructive} disabled={controls_disabled || !!plan.error} onClick={() => { setMessage(null); setDeleteOpen(true); }}>
              {active_plan.deleting ? "重试删除" : "删除计划"}
            </Button>
          </div>
          <PlanDefinition plan={active_plan} workspace={workspaces.value?.find((item) => item.workspace_id === active_plan.settings.workspace_id)?.label} />
          <section className={styles.history} aria-label="执行记录">
            <header className={styles.section_header}>
              <h2>执行记录</h2><InlineIconButton icon="refresh" label="刷新执行记录" onClick={refresh} />
            </header>
            {history.value && !visible_executions.length && <p className={styles.empty}>暂无执行记录</p>}
            {visible_executions.length > 0 && <ul className={styles.executions}>
              {visible_executions.map((item) => {
                const expanded = execution_id === item.execution_id;
                const panel_id = `${execution_panel_prefix}-${item.execution_id}`;
                const status = expanded ? current.value?.status ?? item.status : item.status;
                const accepted_at = new Date(item.accepted_at_ms);
                return <li key={item.execution_id}>
                  <button type="button" className={styles.execution_row} aria-expanded={expanded} aria-controls={expanded ? panel_id : undefined}
                    ref={(button) => {
                      if (!button || focus_execution.current !== item.execution_id) return;
                      focus_execution.current = null;
                      button.focus({ preventScroll: true });
                      button.scrollIntoView({ block: "nearest" });
                    }}
                    onClick={() => { setExecutionId(expanded ? null : item.execution_id); setMessage(null); }}>
                    <time dateTime={accepted_at.toISOString()}>{accepted_at.toLocaleString("zh-CN", { hour12: false })}</time>
                    <span>{item.scheduled_for_ms === null ? "手动" : "定时"}</span>
                    <strong>{executionStatus[status]}</strong><Icon name="chevron-down" size={14} />
                  </button>
                  <Collapse open={expanded} id={panel_id}>
                    {expanded && <>
                      {current.error && <div className={styles.error} role="alert">
                        {current.missing ? "此执行记录已不可用。" : current.error}
                        <Button size="small" variant="text" onClick={refresh}>刷新</Button>
                      </div>}
                      {current.value ? renderExecution(current.value)
                        : !current.error && <p className={styles.execution_loading} role="status">正在读取执行结果…</p>}
                    </>}
                  </Collapse>
                </li>;
              })}
            </ul>}
            {execution_id && !selected_in_history && !current.value && !current.error && <p role="status">正在读取执行结果…</p>}
            <Pagination label="执行记录" page={history.value?.page} on_change={(next) => { setExecutionId(null); setHistoryOffset(next); }} />
          </section>
        </>}
        {delete_open && (active_plan || schedule_id) && <DeleteDialog
          name={active_plan?.settings.name} busy={!!busy} message={message} needs_verify={!!reconcile}
          connected={connected} on_cancel={() => { setDeleteOpen(false); setMessage(null); }}
          on_verify={() => { if (reconcile) void mutate("verify", () => verify(reconcile), false); }}
          on_delete={() => { if (schedule_id) remove(schedule_id); }} />}
      </div>
    </div>
  </>;
}

function Pagination({ label, page, on_change }: Readonly<{
  label: string; page?: ContextPage; on_change: (offset: number) => void;
}>) {
  if (!page || page.total <= page.limit) return null;
  return <nav className={styles.pagination} aria-label={`${label}分页`}>
    <InlineIconButton icon="chevron-left" label={`上一页${label}`} disabled={page.offset === 0}
      onClick={() => on_change(Math.max(0, page.offset - page.limit))} />
    <span>{Math.floor(page.offset / page.limit) + 1} / {Math.ceil(page.total / page.limit)}</span>
    <InlineIconButton icon="chevron-right" label={`下一页${label}`} disabled={page.offset + page.limit >= page.total}
      onClick={() => on_change(page.offset + page.limit)} />
  </nav>;
}

function DeleteDialog(props: Readonly<{
  name?: string; busy: boolean; connected: boolean; message: string | null; needs_verify: boolean;
  on_cancel: () => void; on_delete: () => void; on_verify: () => void;
}>) {
  const title_id = useId();
  const description_id = useId();
  const cancel = useRef<HTMLButtonElement>(null);
  return <Dialog aria_labelledby={title_id} aria_describedby={description_id}
    backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog}
    initial_focus_ref={cancel} dismissible={!props.busy} on_close={props.on_cancel}>
    <h2 id={title_id}>{props.name ? `删除「${props.name}」？` : "删除计划？"}</h2>
    <p id={description_id}>正在运行的任务会停止。计划、执行记录及未投递的归档文件将被删除，已汇报到主控会话的结果和文件仍可查看。</p>
    {props.message && <p role="alert" className={styles.error}>{props.message}</p>}
    <footer className={styles.actions}>
      <Button ref={cancel} disabled={props.busy} onClick={props.on_cancel}>取消</Button>
      {props.needs_verify
        ? <Button disabled={props.busy || !props.connected} onClick={props.on_verify}>刷新并核对</Button>
        : <Button disabled={props.busy || !props.connected} variant="danger" onClick={props.on_delete}>
          {props.busy ? "正在删除…" : "删除计划"}
        </Button>}
    </footer>
  </Dialog>;
}
