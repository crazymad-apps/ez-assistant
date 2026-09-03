import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useState } from "react";
import type {
  AgentVariant,
  PermissionCommandMatch,
  PermissionDocumentSnapshot,
  PermissionFileOperationDefinition,
  PermissionPathMatch,
  PermissionProcessModeDefinition,
  PermissionRuleDefinition,
  PermissionRuleEffect,
} from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { Icon } from "../../../components/Icon";
import { PresenceBoundary } from "../../../components/Presence";
import {
  SelectionPopover,
  type SelectionOption,
} from "../../../components/SelectionPopover";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import styles from "./index.module.scss";

type MatcherType = "general" | "file" | "shell" | "mcp";
type RulePicker = "effect" | "matcher_type" | "file_operation" | "path_match" | "command_match" | "process_mode" | "mcp_scope";
type McpScope = "tool" | "server" | "all";
const MCP_SCOPE_OPTIONS: readonly SelectionOption<McpScope>[] = [
  { value: "tool", label: "指定服务的单个工具" },
  { value: "server", label: "指定服务的全部工具" },
  { value: "all", label: "全部 MCP 工具" },
];

const EFFECT_OPTIONS: readonly SelectionOption<PermissionRuleEffect>[] = [
  { value: "allow", label: "允许" },
  { value: "ask", label: "询问" },
  { value: "deny", label: "拒绝" },
];
const MATCHER_OPTIONS: readonly SelectionOption<MatcherType>[] = [
  { value: "file", label: "文件操作" },
  { value: "shell", label: "命令行" },
  { value: "general", label: "通用工具" },
  { value: "mcp", label: "MCP 工具" },
];
const FILE_OPERATION_OPTIONS: readonly SelectionOption<PermissionFileOperationDefinition>[] = [
  { value: "read", label: "读取文件" },
  { value: "list", label: "列出目录" },
  { value: "find", label: "查找路径" },
  { value: "search", label: "搜索内容" },
  { value: "write", label: "写入文件" },
  { value: "edit", label: "编辑文件" },
  { value: "delete", label: "删除文件" },
];
const PATH_MATCH_OPTIONS: readonly SelectionOption<PermissionPathMatch>[] = [
  { value: "exact", label: "仅当前路径" },
  { value: "recursive", label: "包含子目录" },
];
const COMMAND_MATCH_OPTIONS: readonly SelectionOption<PermissionCommandMatch>[] = [
  { value: "exact", label: "完整命令" },
  { value: "prefix", label: "命令前缀" },
];
const PROCESS_MODE_OPTIONS: readonly SelectionOption<PermissionProcessModeDefinition>[] = [
  { value: "managed", label: "受管进程" },
  { value: "detached", label: "后台进程" },
];

type RuleForm = {
  id: string;
  effect: PermissionRuleEffect;
  variants: AgentVariant[];
  matcher_type: MatcherType;
  tool_name: string;
  mcp_scope: McpScope;
  server_key: string;
  file_operation: PermissionFileOperationDefinition;
  path: string;
  path_match: PermissionPathMatch;
  command: string;
  command_match: PermissionCommandMatch;
  working_directory: string;
  process_mode: PermissionProcessModeDefinition;
};

type Props = Readonly<{ onDirtyChange: (dirty: boolean) => void }>;

export const PermissionSettingsPage = observer(function PermissionSettingsPage(props: Props) {
  const settings = useRootStore().settings;
  const { onDirtyChange } = props;
  const [selected_key, setSelectedKey] = useState("session");
  const [editing, setEditing] = useState<RuleForm | null>(null);
  const [pending_scope_key, setPendingScopeKey] = useState<string | null>(null);
  const [deleting_rule, setDeletingRule] = useState<PermissionRuleDefinition | null>(null);
  const documents = settings.permission_documents;
  const selected = useMemo(
    () => documents.find((document) => scopeKey(document) === selected_key) ?? documents[0] ?? null,
    [documents, selected_key],
  );

  useEffect(() => {
    if (selected && !documents.some((document) => scopeKey(document) === selected_key)) {
      setSelectedKey(scopeKey(selected));
    }
  }, [documents, selected, selected_key]);

  useEffect(() => {
    onDirtyChange(editing !== null);
    return () => onDirtyChange(false);
  }, [editing, onDirtyChange]);

  async function saveRule() {
    if (!selected || !editing) return;
    const rule = buildRule(editing);
    if (!rule) {
      settings.showError("请完整填写当前规则的匹配条件，并至少选择一种模式。");
      return;
    }
    const rules = selected.rules.some((candidate) => candidate.id === rule.id)
      ? selected.rules.map((candidate) => candidate.id === rule.id ? rule : candidate)
      : [...selected.rules, rule];
    const saved = await settings.replacePermissionDocument(
      selected.scope,
      selected.revision,
      { schema_version: selected.schema_version, rules },
    );
    if (saved) setEditing(null);
  }

  async function confirmDeleteRule() {
    if (!selected || !deleting_rule) return;
    const deleted = await settings.replacePermissionDocument(
      selected.scope,
      selected.revision,
      {
        schema_version: selected.schema_version,
        rules: selected.rules.filter((candidate) => candidate.id !== deleting_rule.id),
      },
    );
    if (deleted) setDeletingRule(null);
  }

  function selectScope(scope_key: string) {
    if (editing) {
      setPendingScopeKey(scope_key);
      return;
    }
    setSelectedKey(scope_key);
  }

  function confirmScopeChange() {
    if (!pending_scope_key) return;
    setEditing(null);
    setSelectedKey(pending_scope_key);
    setPendingScopeKey(null);
  }

  return (
    <SettingsPageContainer
      actions={(
        <>
          <div className={styles.permission_scope_tabs} role="tablist">
            {documents.map((document) => (
              <button
                aria-selected={scopeKey(document) === selected_key}
                key={scopeKey(document)}
                onClick={() => selectScope(scopeKey(document))}
                role="tab"
                type="button"
              >
                {scopeLabel(document)}
              </button>
            ))}
          </div>
          <button
            disabled={settings.pending_action === "permission:reload"}
            onClick={() => void settings.reloadPermissions()}
            type="button"
          >
            <Icon name="refresh" size={14} />重新加载
          </button>
        </>
      )}
      title="权限"
    >
      {!selected && (
        <div className={styles.permission_empty}>选择一个会话后，可查看会话与工作区权限。</div>
      )}

      {selected && (
        <>
          <div className={styles.permission_summary}>
            <div>
              <strong>{scopeLabel(selected)}规则</strong>
              <span data-status={selected.status}>{statusLabel(selected.status)}</span>
            </div>
            {selected.editable && !editing && (
              <button onClick={() => setEditing(emptyRuleForm())} type="button">
                <Icon name="plus" size={14} />添加规则
              </button>
            )}
          </div>

          {selected.diagnostics.length > 0 && (
            <div className={styles.permission_diagnostics}>
              {selected.diagnostics.map((diagnostic, index) => (
                <p key={`${diagnostic.code}-${index}`}>{diagnostic.message}</p>
              ))}
            </div>
          )}

          {settings.permission_conflict && (
            <div className={styles.permission_conflict}>
              <span>权限文件已被外部修改，本次保存没有覆盖新内容。</span>
              <button onClick={() => void settings.loadPermissions()} type="button">加载最新内容</button>
            </div>
          )}

          {editing ? (
            <RuleEditor
              form={editing}
              onCancel={() => setEditing(null)}
              onChange={setEditing}
              onSave={() => void saveRule()}
              pending={settings.pending_action === "permission:save"}
            />
          ) : (
            <div className={styles.permission_rule_list}>
              {selected.rules.map((rule) => (
                <article key={rule.id}>
                  <span className={styles.permission_effect} data-effect={rule.effect}>
                    {effectLabel(rule.effect)}
                  </span>
                  <div>
                    <strong>{matcherTitle(rule)}</strong>
                    <small>{matcherDescription(rule)} · {rule.variants.map(variantLabel).join(" / ")}</small>
                  </div>
                  {selected.editable && (
                    <div className={styles.permission_rule_actions}>
                      <button aria-label="编辑规则" onClick={() => setEditing(formFromRule(rule))} type="button">
                        <Icon name="edit" size={14} />
                      </button>
                      <button aria-label="删除规则" onClick={() => setDeletingRule(rule)} type="button">
                        <Icon name="trash" size={14} />
                      </button>
                    </div>
                  )}
                </article>
              ))}
              {selected.rules.length === 0 && (
                <p className={styles.permission_empty}>当前范围没有显式规则，将继续由下一层规则和审批模式判断。</p>
              )}
            </div>
          )}
        </>
      )}
      <PresenceBoundary present={pending_scope_key !== null}>
        {pending_scope_key && (
          <SessionActionDialog
            confirm_label="切换范围"
            is_danger
            is_pending={false}
            on_cancel={() => setPendingScopeKey(null)}
            on_confirm={confirmScopeChange}
            title="放弃未保存的规则修改？"
          >
            <p>当前规则尚未保存，继续后这些修改将丢失。</p>
          </SessionActionDialog>
        )}
      </PresenceBoundary>
      <PresenceBoundary present={deleting_rule !== null}>
        {deleting_rule && (
          <SessionActionDialog
            confirm_label="删除规则"
            is_danger
            is_pending={settings.pending_action === "permission:save"}
            on_cancel={() => setDeletingRule(null)}
            on_confirm={() => void confirmDeleteRule()}
            title={`删除规则“${deleting_rule.id}”？`}
          >
            <p>删除后，相关操作将继续由下一层权限规则和审批模式判断。</p>
          </SessionActionDialog>
        )}
      </PresenceBoundary>
    </SettingsPageContainer>
  );
});

function RuleEditor(props: Readonly<{
  form: RuleForm;
  onCancel: () => void;
  onChange: (form: RuleForm) => void;
  onSave: () => void;
  pending: boolean;
}>) {
  const [open_picker, setOpenPicker] = useState<RulePicker | null>(null);
  const update = <Key extends keyof RuleForm>(key: Key, value: RuleForm[Key]) => {
    props.onChange({ ...props.form, [key]: value });
  };
  const toggleVariant = (variant: AgentVariant) => {
    update("variants", props.form.variants.includes(variant)
      ? props.form.variants.filter((candidate) => candidate !== variant)
      : [...props.form.variants, variant]);
  };
  return (
    <div className={styles.permission_editor}>
      <div className={styles.permission_editor_header}>
        <strong>{props.form.id.startsWith("ui-") ? "添加规则" : "编辑规则"}</strong>
        <span>规则按会话 → 工作区 → 全局顺序匹配，拒绝始终优先。</span>
      </div>
      <div className={styles.permission_form}>
        <PermissionSelect
          aria_label="选择规则效果"
          label="效果"
          on_open_change={(open) => setOpenPicker(open ? "effect" : null)}
          on_select={(value) => update("effect", value)}
          open={open_picker === "effect"}
          options={EFFECT_OPTIONS}
          selected={props.form.effect}
        />
        <PermissionSelect
          aria_label="选择匹配类型"
          label="匹配类型"
          on_open_change={(open) => setOpenPicker(open ? "matcher_type" : null)}
          on_select={(value) => update("matcher_type", value)}
          open={open_picker === "matcher_type"}
          options={MATCHER_OPTIONS}
          selected={props.form.matcher_type}
        />
        {props.form.matcher_type === "general" && (
          <label className={styles.full_field}>工具名称<input onChange={(event) => update("tool_name", event.currentTarget.value)} placeholder="例如 delegate_task" value={props.form.tool_name} /></label>
        )}
        {props.form.matcher_type === "mcp" && <>
          <PermissionSelect
            aria_label="选择 MCP 匹配范围"
            label="MCP 范围"
            on_open_change={(open) => setOpenPicker(open ? "mcp_scope" : null)}
            on_select={(value) => update("mcp_scope", value)}
            open={open_picker === "mcp_scope"}
            options={MCP_SCOPE_OPTIONS}
            selected={props.form.mcp_scope}
          />
          {props.form.mcp_scope !== "all" && <label className={styles.full_field}>服务 key<input onChange={(event) => update("server_key", event.currentTarget.value)} placeholder="例如 github，不使用展示名称" value={props.form.server_key} /></label>}
          {props.form.mcp_scope === "tool" && <label className={styles.full_field}>原始工具名称<input onChange={(event) => update("tool_name", event.currentTarget.value)} placeholder="例如 create_issue，不使用网关名称" value={props.form.tool_name} /></label>}
          <p className={styles.full_field}>MCP 权限独立于文件和 Shell；工具注解不参与授权判断。</p>
        </>}
        {props.form.matcher_type === "file" && (
          <>
            <PermissionSelect
              aria_label="选择文件操作"
              label="文件操作"
              on_open_change={(open) => setOpenPicker(open ? "file_operation" : null)}
              on_select={(value) => update("file_operation", value)}
              open={open_picker === "file_operation"}
              options={FILE_OPERATION_OPTIONS}
              selected={props.form.file_operation}
            />
            <PermissionSelect
              aria_label="选择路径范围"
              label="路径范围"
              on_open_change={(open) => setOpenPicker(open ? "path_match" : null)}
              on_select={(value) => update("path_match", value)}
              open={open_picker === "path_match"}
              options={PATH_MATCH_OPTIONS}
              selected={props.form.path_match}
            />
            <label className={styles.full_field}>绝对路径<input onChange={(event) => update("path", event.currentTarget.value)} placeholder="例如 /Users/me/project" value={props.form.path} /></label>
          </>
        )}
        {props.form.matcher_type === "shell" && (
          <>
            <PermissionSelect
              aria_label="选择命令匹配方式"
              label="命令匹配"
              on_open_change={(open) => setOpenPicker(open ? "command_match" : null)}
              on_select={(value) => update("command_match", value)}
              open={open_picker === "command_match"}
              options={COMMAND_MATCH_OPTIONS}
              selected={props.form.command_match}
            />
            <PermissionSelect
              aria_label="选择进程方式"
              label="进程方式"
              on_open_change={(open) => setOpenPicker(open ? "process_mode" : null)}
              on_select={(value) => update("process_mode", value)}
              open={open_picker === "process_mode"}
              options={PROCESS_MODE_OPTIONS}
              selected={props.form.process_mode}
            />
            <label className={styles.full_field}>命令<input onChange={(event) => update("command", event.currentTarget.value)} placeholder="例如 npm test" value={props.form.command} /></label>
            <label className={styles.full_field}>工作目录<input onChange={(event) => update("working_directory", event.currentTarget.value)} placeholder="例如 /Users/me/project" value={props.form.working_directory} /></label>
          </>
        )}
        <fieldset className={styles.permission_variants}>
          <legend>适用模式</legend>
          <label><input checked={props.form.variants.includes("build")} onChange={() => toggleVariant("build")} type="checkbox" />构建</label>
          <label><input checked={props.form.variants.includes("plan")} onChange={() => toggleVariant("plan")} type="checkbox" />规划</label>
        </fieldset>
      </div>
      <div className={styles.permission_editor_actions}>
        <button onClick={props.onCancel} type="button">取消</button>
        <button className={styles.primary_button} disabled={props.pending} onClick={props.onSave} type="button">保存规则</button>
      </div>
    </div>
  );
}

function PermissionSelect<T extends string>(props: Readonly<{
  aria_label: string;
  label: string;
  on_open_change: (open: boolean) => void;
  on_select: (value: T) => void;
  open: boolean;
  options: readonly SelectionOption<T>[];
  selected: T;
}>) {
  return (
    <div className={styles.permission_field}>
      <span>{props.label}</span>
      <SelectionPopover
        aria_label={props.aria_label}
        content_width="content"
        on_open_change={props.on_open_change}
        on_select={props.on_select}
        open={props.open}
        options={props.options}
        selected={props.selected}
        trigger_class_name={styles.permission_select}
        trigger_variant="field"
      />
    </div>
  );
}

function emptyRuleForm(): RuleForm {
  return {
    id: `ui-${Date.now().toString(36)}`,
    effect: "allow",
    variants: ["build"],
    matcher_type: "file",
    tool_name: "",
    mcp_scope: "tool",
    server_key: "",
    file_operation: "read",
    path: "",
    path_match: "recursive",
    command: "",
    command_match: "exact",
    working_directory: "",
    process_mode: "managed",
  };
}

function formFromRule(rule: PermissionRuleDefinition): RuleForm {
  const form = emptyRuleForm();
  form.id = rule.id;
  form.effect = rule.effect;
  form.variants = [...rule.variants];
  form.matcher_type = rule.matcher.type;
  if (rule.matcher.type === "mcp") {
    const { server, tool } = rule.matcher.payload;
    form.mcp_scope = server.type === "any" ? "all" : "server";
    if (tool.type === "exact") { form.mcp_scope = "tool"; form.tool_name = tool.payload.tool_name; }
    if (server.type === "exact") form.server_key = server.payload.server_key;
  }
  if (rule.matcher.type === "general") form.tool_name = rule.matcher.payload.tool_name;
  if (rule.matcher.type === "file") Object.assign(form, {
    file_operation: rule.matcher.payload.operation,
    path: rule.matcher.payload.path,
    path_match: rule.matcher.payload.path_match,
  });
  if (rule.matcher.type === "shell") Object.assign(form, rule.matcher.payload);
  return form;
}

function buildRule(form: RuleForm): PermissionRuleDefinition | null {
  if (form.variants.length === 0) return null;
  if (form.matcher_type === "mcp") {
    if (form.mcp_scope !== "all" && !form.server_key.trim()) return null;
    if (form.mcp_scope === "tool" && !form.tool_name.trim()) return null;
    return { id: form.id, effect: form.effect, variants: form.variants, matcher: { type: "mcp", payload: {
      server: form.mcp_scope === "all" ? { type: "any" } : { type: "exact", payload: { server_key: form.server_key.trim() } },
      tool: form.mcp_scope === "tool" ? { type: "exact", payload: { tool_name: form.tool_name.trim() } } : { type: "any" },
    } } };
  }
  if (form.matcher_type === "general") {
    if (!form.tool_name.trim()) return null;
    return { id: form.id, effect: form.effect, variants: form.variants, matcher: { type: "general", payload: { tool_name: form.tool_name.trim() } } };
  }
  if (form.matcher_type === "file") {
    if (!form.path.trim()) return null;
    return { id: form.id, effect: form.effect, variants: form.variants, matcher: { type: "file", payload: { operation: form.file_operation, path: form.path.trim(), path_match: form.path_match } } };
  }
  if (!form.command.trim() || !form.working_directory.trim()) return null;
  return { id: form.id, effect: form.effect, variants: form.variants, matcher: { type: "shell", payload: { command: form.command.trim(), command_match: form.command_match, working_directory: form.working_directory.trim(), process_mode: form.process_mode } } };
}

function scopeKey(document: PermissionDocumentSnapshot): string { return document.scope.type; }
function scopeLabel(document: PermissionDocumentSnapshot): string {
  return document.scope.type === "session" ? "当前会话" : document.scope.type === "workspace" ? "工作区" : "全局";
}
function statusLabel(status: PermissionDocumentSnapshot["status"]): string {
  return ({ empty: "未配置", ready: "已生效", invalid: "文件无效", unavailable: "不可用" })[status];
}
function effectLabel(effect: PermissionRuleEffect): string { return ({ allow: "允许", ask: "询问", deny: "拒绝" })[effect]; }
function variantLabel(variant: AgentVariant): string { return variant === "build" ? "构建" : "规划"; }
function matcherTitle(rule: PermissionRuleDefinition): string {
  if (rule.matcher.type === "mcp") {
    const { server, tool } = rule.matcher.payload;
    return `${server.type === "any" ? "全部服务" : server.payload.server_key} / ${tool.type === "any" ? "全部工具" : tool.payload.tool_name}`;
  }
  if (rule.matcher.type === "general") return rule.matcher.payload.tool_name;
  if (rule.matcher.type === "file") return `${rule.matcher.payload.operation} · ${rule.matcher.payload.path}`;
  return rule.matcher.payload.command;
}
function matcherDescription(rule: PermissionRuleDefinition): string {
  if (rule.matcher.type === "mcp") return "MCP 工具";
  if (rule.matcher.type === "general") return "通用工具";
  if (rule.matcher.type === "file") return rule.matcher.payload.path_match === "recursive" ? "包含子目录" : "仅当前路径";
  return `${rule.matcher.payload.command_match === "prefix" ? "前缀" : "完整"} · ${rule.matcher.payload.working_directory}`;
}
