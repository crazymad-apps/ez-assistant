import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  AgentVariant,
  PermissionCommandMatch,
  PermissionDocumentSnapshot,
  PermissionFileOperationDefinition,
  PermissionGrantSnapshot,
  PermissionPathMatch,
  PermissionProcessModeDefinition,
  PermissionRuleDefinition,
  PermissionRuleEffect,
  PermissionRuleMatcher,
} from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { Icon } from "../../../components/Icon";
import { useFlipIndicator } from "../../../components/Motion";
import { PresenceBoundary } from "../../../components/Presence";
import {
  SelectionPopover,
  type SelectionOption,
} from "../../../components/SelectionPopover";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import styles from "./index.module.scss";

type MatcherType = "general" | "file" | "shell" | "mcp";
type RulePicker = "effect" | "matcher_type" | "file_operation" | "path_match" | "command_match" | "process_mode" | "mcp_scope" | "ttl";
type McpScope = "tool" | "server" | "all";
type GrantTtlChoice = "600" | "3600" | "86400" | "604800" | "never";
const MCP_SCOPE_OPTIONS: readonly SelectionOption<McpScope>[] = [
  { value: "tool", label: "指定服务的单个工具" },
  { value: "server", label: "指定服务的全部工具" },
  { value: "all", label: "全部 MCP 工具" },
];
const GRANT_TTL_OPTIONS: readonly SelectionOption<GrantTtlChoice>[] = [
  { value: "600", label: "10 分钟" },
  { value: "3600", label: "1 小时" },
  { value: "86400", label: "24 小时" },
  { value: "604800", label: "7 天" },
  { value: "never", label: "永不过期" },
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
  { value: "managed", label: "工具托管", description: "返回前收敛可管理的进程树" },
  { value: "background", label: "后台运行", description: "由 Runtime 持续管理，可查输出和停止" },
];
const LEGACY_DETACHED_OPTION: SelectionOption<PermissionProcessModeDefinition> = { value: "detached", label: "完成后交接（历史规则）" };

type MatcherForm = {
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

type RuleForm = MatcherForm & {
  id: string;
  effect: PermissionRuleEffect;
  variants: AgentVariant[];
};

type GrantForm = MatcherForm & {
  variants: AgentVariant[];
  max_uses_text: string;
  ttl: GrantTtlChoice;
  note: string;
};

type Props = Readonly<{ onDirtyChange: (dirty: boolean) => void }>;

export const PermissionSettingsPage = observer(function PermissionSettingsPage(props: Props) {
  const settings = useRootStore().settings;
  const { onDirtyChange } = props;
  const [selected_key, setSelectedKey] = useState("session");
  const [editing, setEditing] = useState<RuleForm | null>(null);
  const [grant_editing, setGrantEditing] = useState<GrantForm | null>(null);
  const [pending_scope_key, setPendingScopeKey] = useState<string | null>(null);
  const [deleting_rule, setDeletingRule] = useState<PermissionRuleDefinition | null>(null);
  const scope_tabs_ref = useRef<HTMLDivElement>(null);
  const scope_indicator_ref = useRef<HTMLSpanElement>(null);
  const documents = settings.permission_documents;
  const selected = useMemo(
    () => documents.find((document) => scopeKey(document) === selected_key) ?? documents[0] ?? null,
    [documents, selected_key],
  );
  const scope_note = selected ? scopeNote(selected) : null;
  const active_scope_key = selected ? scopeKey(selected) : "";

  useFlipIndicator({
    active_key: active_scope_key,
    active_selector: 'button[aria-selected="true"]',
    container_ref: scope_tabs_ref,
    indicator_ref: scope_indicator_ref,
  });

  useEffect(() => {
    if (selected && !documents.some((document) => scopeKey(document) === selected_key)) {
      setSelectedKey(scopeKey(selected));
    }
  }, [documents, selected, selected_key]);

  useEffect(() => {
    onDirtyChange(editing !== null || grant_editing !== null);
    return () => onDirtyChange(false);
  }, [editing, grant_editing, onDirtyChange]);

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

  async function saveGrant() {
    if (!grant_editing) return;
    if (grant_editing.variants.length === 0) {
      settings.showError("请至少选择一种适用模式。");
      return;
    }
    const matcher = buildMatcher(grant_editing);
    if (!matcher) {
      settings.showError("请完整填写预授权的工具匹配条件。");
      return;
    }
    const max_uses = Number(grant_editing.max_uses_text);
    if (!Number.isInteger(max_uses) || max_uses < 1 || max_uses > 100) {
      settings.showError("可用次数必须为 1 到 100 之间的整数。");
      return;
    }
    const ttl_seconds = grant_editing.ttl === "never" ? undefined : Number(grant_editing.ttl);
    const saved = await settings.createPermissionGrant({
      variants: grant_editing.variants,
      matcher,
      max_uses,
      ttl_seconds,
      note: grant_editing.note.trim(),
    });
    if (saved) setGrantEditing(null);
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
    if (editing || grant_editing) {
      setPendingScopeKey(scope_key);
      return;
    }
    setSelectedKey(scope_key);
  }

  function confirmScopeChange() {
    if (!pending_scope_key) return;
    setEditing(null);
    setGrantEditing(null);
    setSelectedKey(pending_scope_key);
    setPendingScopeKey(null);
  }

  return (
    <SettingsPageContainer
      actions={(
        <>
          <div className={styles.permission_scope_tabs} ref={scope_tabs_ref} role="tablist">
            <span aria-hidden="true" className={styles.permission_scope_indicator} ref={scope_indicator_ref} />
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
            onClick={() => void settings.reloadPermissions(selected?.scope.type)}
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
              <button
                disabled={grant_editing !== null}
                onClick={() => setEditing(emptyRuleForm())}
                type="button"
              >
                <Icon name="plus" size={14} />添加规则
              </button>
            )}
          </div>

          {scope_note && (
            <p className={styles.permission_scope_note}>{scope_note}</p>
          )}

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
              is_controller={selected.scope.type === "controller"}
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
                    <strong>{matcherTitle(rule.matcher)}</strong>
                    <small>{matcherDescription(rule.matcher)} · {rule.variants.map(variantLabel).join(" / ")}</small>
                  </div>
                  {selected.editable && (
                    <div className={styles.permission_rule_actions}>
                      <button
                        aria-label="编辑规则"
                        disabled={grant_editing !== null}
                        onClick={() => setEditing(formFromRule(rule))}
                        type="button"
                      >
                        <Icon name="edit" size={14} />
                      </button>
                      <button
                        aria-label="删除规则"
                        disabled={grant_editing !== null}
                        onClick={() => setDeletingRule(rule)}
                        type="button"
                      >
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

          {selected.scope.type === "controller" && (
            <section className={styles.permission_grants_section}>
              <div className={styles.permission_summary}>
                <div>
                  <strong>后台一次性预授权</strong>
                  <span>{settings.permission_grants.length} 条生效中</span>
                </div>
                {!grant_editing && (
                  <button
                    disabled={editing !== null}
                    onClick={() => setGrantEditing(emptyGrantForm())}
                    type="button"
                  >
                    <Icon name="plus" size={14} />新增预授权
                  </button>
                )}
              </div>

              {grant_editing ? (
                <GrantEditor
                  form={grant_editing}
                  onCancel={() => setGrantEditing(null)}
                  onChange={setGrantEditing}
                  onSave={() => void saveGrant()}
                  pending={settings.pending_action === "permission_grant:create"}
                />
              ) : (
                <div className={styles.permission_rule_list}>
                  {settings.permission_grants.map((grant) => (
                    <article className={styles.permission_grant_row} key={grant.grant_id}>
                      <span className={styles.permission_grant_badge}>
                        剩余 {grant.remaining_uses} / {grant.max_uses} 次
                      </span>
                      <div>
                        <strong>{matcherTitle(grant.matcher)}</strong>
                        <small>{grantDescription(grant)}</small>
                      </div>
                      <div className={styles.permission_rule_actions}>
                        <button
                          aria-label="撤销预授权"
                          disabled={settings.pending_action === "permission_grant:revoke"}
                          onClick={() => void settings.revokePermissionGrant(grant.grant_id)}
                          type="button"
                        >
                          <Icon name="trash" size={14} />
                        </button>
                      </div>
                    </article>
                  ))}
                  {settings.permission_grants.length === 0 && (
                    <p className={styles.permission_empty}>当前没有生效中的一次性预授权。</p>
                  )}
                </div>
              )}
            </section>
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
  is_controller: boolean;
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
        <span>
          {props.is_controller
            ? "助理会话仅匹配会话与助理会话规则，拒绝始终优先，与普通会话的全局规则隔离。"
            : "规则按会话 → 工作区 → 全局顺序匹配，拒绝始终优先。"}
        </span>
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
        <MatcherFields
          form={props.form}
          on_change={(patch) => props.onChange({ ...props.form, ...patch })}
          on_picker_change={setOpenPicker}
          open_picker={open_picker}
        />
        <VariantFieldset on_toggle={toggleVariant} variants={props.form.variants} />
      </div>
      <div className={styles.permission_editor_actions}>
        <button onClick={props.onCancel} type="button">取消</button>
        <button className={styles.primary_button} disabled={props.pending} onClick={props.onSave} type="button">保存规则</button>
      </div>
    </div>
  );
}

function GrantEditor(props: Readonly<{
  form: GrantForm;
  onCancel: () => void;
  onChange: (form: GrantForm) => void;
  onSave: () => void;
  pending: boolean;
}>) {
  const [open_picker, setOpenPicker] = useState<RulePicker | null>(null);

  const update = <Key extends keyof GrantForm>(key: Key, value: GrantForm[Key]) => {
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
        <strong>新增预授权</strong>
        <span>预授权仅对助理会话生效，按创建时间先进先出消耗次数，显式拒绝规则始终优先。</span>
      </div>
      <div className={styles.permission_form}>
        <PermissionSelect
          aria_label="选择有效期"
          label="有效期"
          on_open_change={(open) => setOpenPicker(open ? "ttl" : null)}
          on_select={(value) => update("ttl", value)}
          open={open_picker === "ttl"}
          options={GRANT_TTL_OPTIONS}
          selected={props.form.ttl}
        />
        <label>
          可用次数
          <input
            aria-label="可用次数"
            max={100}
            min={1}
            onChange={(event) => update("max_uses_text", event.currentTarget.value)}
            type="number"
            value={props.form.max_uses_text}
          />
        </label>
        <MatcherFields
          form={props.form}
          on_change={(patch) => props.onChange({ ...props.form, ...patch })}
          on_picker_change={setOpenPicker}
          open_picker={open_picker}
        />
        <label className={styles.full_field}>
          备注
          <input
            aria-label="预授权备注"
            maxLength={256}
            onChange={(event) => update("note", event.currentTarget.value)}
            placeholder="例如：允许语音触发一次重启或读取日志"
            value={props.form.note}
          />
        </label>
        <VariantFieldset on_toggle={toggleVariant} variants={props.form.variants} />
      </div>
      <div className={styles.permission_editor_actions}>
        <button onClick={props.onCancel} type="button">取消</button>
        <button className={styles.primary_button} disabled={props.pending} onClick={props.onSave} type="button">保存预授权</button>
      </div>
    </div>
  );
}

function MatcherFields(props: Readonly<{
  form: MatcherForm;
  open_picker: RulePicker | null;
  on_picker_change: (picker: RulePicker | null) => void;
  on_change: (patch: Partial<MatcherForm>) => void;
}>) {
  return (
    <>
      <PermissionSelect
        aria_label="选择匹配类型"
        label="匹配类型"
        on_open_change={(open) => props.on_picker_change(open ? "matcher_type" : null)}
        on_select={(value) => props.on_change({ matcher_type: value })}
        open={props.open_picker === "matcher_type"}
        options={MATCHER_OPTIONS}
        selected={props.form.matcher_type}
      />
      {props.form.matcher_type === "general" && (
        <label className={styles.full_field}>
          工具名称
          <input
            onChange={(event) => props.on_change({ tool_name: event.currentTarget.value })}
            placeholder="例如 delegate_task"
            value={props.form.tool_name}
          />
        </label>
      )}
      {props.form.matcher_type === "mcp" && (
        <>
          <PermissionSelect
            aria_label="选择 MCP 匹配范围"
            label="MCP 范围"
            on_open_change={(open) => props.on_picker_change(open ? "mcp_scope" : null)}
            on_select={(value) => props.on_change({ mcp_scope: value })}
            open={props.open_picker === "mcp_scope"}
            options={MCP_SCOPE_OPTIONS}
            selected={props.form.mcp_scope}
          />
          {props.form.mcp_scope !== "all" && (
            <label className={styles.full_field}>
              服务 key
              <input
                onChange={(event) => props.on_change({ server_key: event.currentTarget.value })}
                placeholder="例如 github，不使用展示名称"
                value={props.form.server_key}
              />
            </label>
          )}
          {props.form.mcp_scope === "tool" && (
            <label className={styles.full_field}>
              原始工具名称
              <input
                onChange={(event) => props.on_change({ tool_name: event.currentTarget.value })}
                placeholder="例如 create_issue，不使用网关名称"
                value={props.form.tool_name}
              />
            </label>
          )}
          <p className={styles.full_field}>MCP 权限独立于文件和 Shell；工具注解不参与授权判断。</p>
        </>
      )}
      {props.form.matcher_type === "file" && (
        <>
          <PermissionSelect
            aria_label="选择文件操作"
            label="文件操作"
            on_open_change={(open) => props.on_picker_change(open ? "file_operation" : null)}
            on_select={(value) => props.on_change({ file_operation: value })}
            open={props.open_picker === "file_operation"}
            options={FILE_OPERATION_OPTIONS}
            selected={props.form.file_operation}
          />
          <PermissionSelect
            aria_label="选择路径范围"
            label="路径范围"
            on_open_change={(open) => props.on_picker_change(open ? "path_match" : null)}
            on_select={(value) => props.on_change({ path_match: value })}
            open={props.open_picker === "path_match"}
            options={PATH_MATCH_OPTIONS}
            selected={props.form.path_match}
          />
          <label className={styles.full_field}>
            绝对路径
            <input
              onChange={(event) => props.on_change({ path: event.currentTarget.value })}
              placeholder="例如 /Users/me/project"
              value={props.form.path}
            />
          </label>
        </>
      )}
      {props.form.matcher_type === "shell" && (
        <>
          <PermissionSelect
            aria_label="选择命令匹配方式"
            label="命令匹配"
            on_open_change={(open) => props.on_picker_change(open ? "command_match" : null)}
            on_select={(value) => props.on_change({ command_match: value })}
            open={props.open_picker === "command_match"}
            options={COMMAND_MATCH_OPTIONS}
            selected={props.form.command_match}
          />
          <PermissionSelect
            aria_label="选择进程方式"
            label="进程方式"
            on_open_change={(open) => props.on_picker_change(open ? "process_mode" : null)}
            on_select={(value) => props.on_change({ process_mode: value })}
            open={props.open_picker === "process_mode"}
            options={props.form.process_mode === "detached" ? [...PROCESS_MODE_OPTIONS, LEGACY_DETACHED_OPTION] : PROCESS_MODE_OPTIONS}
            selected={props.form.process_mode}
          />
          <label className={styles.full_field}>
            命令
            <input
              onChange={(event) => props.on_change({ command: event.currentTarget.value })}
              placeholder="例如 npm test"
              value={props.form.command}
            />
          </label>
          <label className={styles.full_field}>
            工作目录
            <input
              onChange={(event) => props.on_change({ working_directory: event.currentTarget.value })}
              placeholder="例如 /Users/me/project"
              value={props.form.working_directory}
            />
          </label>
        </>
      )}
    </>
  );
}

function VariantFieldset(props: Readonly<{
  variants: readonly AgentVariant[];
  on_toggle: (variant: AgentVariant) => void;
}>) {
  return (
    <fieldset className={styles.permission_variants}>
      <legend>适用模式</legend>
      <label><input checked={props.variants.includes("build")} onChange={() => props.on_toggle("build")} type="checkbox" />构建</label>
      <label><input checked={props.variants.includes("plan")} onChange={() => props.on_toggle("plan")} type="checkbox" />规划</label>
    </fieldset>
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

function emptyMatcherForm(): MatcherForm {
  return {
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

function emptyRuleForm(): RuleForm {
  return {
    id: `ui-${Date.now().toString(36)}`,
    effect: "allow",
    variants: ["build"],
    ...emptyMatcherForm(),
  };
}

function emptyGrantForm(): GrantForm {
  return {
    ...emptyMatcherForm(),
    variants: ["build"],
    max_uses_text: "1",
    ttl: "3600",
    note: "",
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

function buildMatcher(form: MatcherForm): PermissionRuleMatcher | null {
  if (form.matcher_type === "mcp") {
    if (form.mcp_scope !== "all" && !form.server_key.trim()) return null;
    if (form.mcp_scope === "tool" && !form.tool_name.trim()) return null;
    return {
      type: "mcp",
      payload: {
        server: form.mcp_scope === "all" ? { type: "any" } : { type: "exact", payload: { server_key: form.server_key.trim() } },
        tool: form.mcp_scope === "tool" ? { type: "exact", payload: { tool_name: form.tool_name.trim() } } : { type: "any" },
      },
    };
  }
  if (form.matcher_type === "general") {
    if (!form.tool_name.trim()) return null;
    return { type: "general", payload: { tool_name: form.tool_name.trim() } };
  }
  if (form.matcher_type === "file") {
    if (!form.path.trim()) return null;
    return { type: "file", payload: { operation: form.file_operation, path: form.path.trim(), path_match: form.path_match } };
  }
  if (!form.command.trim() || !form.working_directory.trim()) return null;
  return {
    type: "shell",
    payload: {
      command: form.command.trim(),
      command_match: form.command_match,
      working_directory: form.working_directory.trim(),
      process_mode: form.process_mode,
    },
  };
}

function buildRule(form: RuleForm): PermissionRuleDefinition | null {
  if (form.variants.length === 0) return null;
  const matcher = buildMatcher(form);
  if (!matcher) return null;
  return { id: form.id, effect: form.effect, variants: form.variants, matcher };
}

function scopeKey(document: PermissionDocumentSnapshot): string { return document.scope.type; }

function scopeLabel(document: PermissionDocumentSnapshot): string {
  if (document.scope.type === "session") return "当前会话";
  if (document.scope.type === "workspace") return "工作区";
  if (document.scope.type === "controller") return "助理会话";
  return "全局";
}

function scopeNote(document: PermissionDocumentSnapshot): string | null {
  if (document.scope.type === "controller") {
    return "助理会话权限与普通会话全局规则完全隔离；设备端发起的工具调用如未命中常驻规则或下方一次性预授权，将直接拒绝而不会挂起等待桌面审批。";
  }
  if (document.scope.type === "global") {
    return "全局规则仅作用于普通桌面会话，与助理会话权限相互隔离。";
  }
  return null;
}

function statusLabel(status: PermissionDocumentSnapshot["status"]): string {
  return ({ empty: "未配置", ready: "已生效", invalid: "文件无效", unavailable: "不可用" })[status];
}

function effectLabel(effect: PermissionRuleEffect): string { return ({ allow: "允许", ask: "询问", deny: "拒绝" })[effect]; }

function variantLabel(variant: AgentVariant): string { return variant === "build" ? "构建" : "规划"; }

function matcherTitle(matcher: PermissionRuleMatcher): string {
  if (matcher.type === "mcp") {
    const { server, tool } = matcher.payload;
    return `${server.type === "any" ? "全部服务" : server.payload.server_key} / ${tool.type === "any" ? "全部工具" : tool.payload.tool_name}`;
  }
  if (matcher.type === "general") return matcher.payload.tool_name;
  if (matcher.type === "file") return `${matcher.payload.operation} · ${matcher.payload.path}`;
  return matcher.payload.command;
}

function matcherDescription(matcher: PermissionRuleMatcher): string {
  if (matcher.type === "mcp") return "MCP 工具";
  if (matcher.type === "general") return "通用工具";
  if (matcher.type === "file") return matcher.payload.path_match === "recursive" ? "包含子目录" : "仅当前路径";
  return `${matcher.payload.command_match === "prefix" ? "前缀" : "完整"} · ${matcher.payload.working_directory}`;
}

function formatGrantExpiry(timestamp_ms: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(new Date(timestamp_ms));
}

function grantDescription(grant: PermissionGrantSnapshot): string {
  const parts = [
    matcherDescription(grant.matcher),
    grant.variants.map(variantLabel).join(" / "),
    grant.expires_at_ms != null ? `有效期至 ${formatGrantExpiry(grant.expires_at_ms)}` : "永不过期",
  ];
  if (grant.note) parts.push(grant.note);
  return parts.join(" · ");
}
