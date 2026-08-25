import { observer } from "mobx-react-lite";
import { useMemo, useState } from "react";
import type {
  SkillDiagnosticSnapshot,
  SkillDetailSnapshot,
  SkillHealthSnapshot,
  SkillSourceSnapshot,
  SkillSummarySnapshot,
  WorkspaceId,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { MarkdownContent } from "../../../components/MarkdownContent";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import { copySkillDirectoryPath, openSkillDirectory } from "../../../native-bridge/skillDirectory";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SettingsMessages } from "./RuntimeSettingsPage";
import { SettingsPageContainer } from "./SettingsPageContainer";
import styles from "./SkillSettingsPage.module.scss";

export const SkillSettingsPage = observer(function SkillSettingsPage() {
  const store = useRootStore();
  const settings = store.settings;
  const workspaces = store.projection.application?.workspaces.filter((item) => item.lifecycle === "active") ?? [];
  const [tab, setTab] = useState<"skills" | "diagnostics">("skills");
  const [scope_open, setScopeOpen] = useState(false);
  const [selected_name, setSelectedName] = useState<string | null>(null);
  const snapshot = settings.skill_management;
  const selected = useMemo(
    () => snapshot?.skills.find((item) => item.name === selected_name) ?? null,
    [selected_name, snapshot?.skills],
  );
  const selected_detail = settings.skill_detail?.skill.name === selected_name
    ? settings.skill_detail
    : null;
  const scope_options: readonly SelectionOption<string>[] = [
    { value: "", label: "仅用户根目录" },
    ...workspaces.map((workspace) => ({
      value: workspace.workspace_id,
      label: workspace.user_directory.split("/").filter(Boolean).at(-1) ?? workspace.user_directory,
    })),
  ];
  const directory_sources: ReadonlyArray<{ source: SkillSourceSnapshot; label: string; workspace: boolean }> = [
    { source: "workspace_ez_assistant", label: "工作区 .ez-assistant/skills", workspace: true },
    { source: "workspace_agents", label: "工作区 .agents/skills", workspace: true },
    { source: "user_ez_assistant", label: "用户目录 .ez-assistant/skills", workspace: false },
    { source: "user_agents", label: "用户目录 .agents/skills", workspace: false },
  ];

  async function accessDirectory(source: SkillSourceSnapshot, copy: boolean) {
    try {
      if (copy) {
        await copySkillDirectoryPath(source, settings.skill_workspace_id);
        settings.showNotice("技能来源目录路径已复制。");
      } else {
        await openSkillDirectory(source, settings.skill_workspace_id);
      }
    } catch (error: unknown) {
      settings.showError(error instanceof Error ? error.message : "无法访问技能来源目录。");
    }
  }

  if (selected) {
    return <SkillDetail
      detail={selected_detail}
      diagnostics={selected_detail?.diagnostics
        ?? snapshot?.diagnostics.filter((item) => item.skill_name === selected.name)
        ?? []}
      loading={settings.skill_detail_loading}
      skill={selected}
      on_back={() => {
        settings.clearSkillDetail();
        setSelectedName(null);
      }}
    />;
  }

  return (
    <SettingsPageContainer
      actions={(
        <div className={styles.header_actions}>
          <SelectionPopover
            aria_label="技能工作区范围"
            disabled={settings.skills_loading}
            content_width="content"
            on_open_change={setScopeOpen}
            on_select={(value) => settings.selectSkillWorkspace(value ? value as WorkspaceId : null)}
            open={scope_open}
            options={scope_options}
            selected={settings.skill_workspace_id ?? ""}
            trigger_class_name={styles.workspace_select}
            trigger_variant="compact"
          />
          <details className={styles.directory_menu}>
            <summary>打开来源目录</summary>
            <div>
              {directory_sources.map((item) => {
                const disabled = item.workspace && !settings.skill_workspace_id;
                return <div key={item.source}>
                  <button disabled={disabled} onClick={() => void accessDirectory(item.source, false)} title={disabled ? "请先选择工作区" : item.label} type="button">
                    <Icon name="folder" size={14} />{item.label}
                  </button>
                  <button aria-label={`复制路径 ${item.label}`} disabled={disabled} onClick={() => void accessDirectory(item.source, true)} type="button"><Icon name="copy" size={13} /></button>
                </div>;
              })}
            </div>
          </details>
        </div>
      )}
      title="技能"
    >
      <div className={styles.tabs} role="tablist">
        <button aria-selected={tab === "skills"} onClick={() => setTab("skills")} role="tab" type="button">
          当前技能 <b>{snapshot?.skills.length ?? 0}</b>
        </button>
        <button aria-selected={tab === "diagnostics"} onClick={() => setTab("diagnostics")} role="tab" type="button">
          诊断 <b>{snapshot?.diagnostics.length ?? 0}</b>
        </button>
      </div>
      {settings.skills_loading && !snapshot ? <p className={styles.empty}>正在读取技能…</p> : null}
      {!settings.skills_loading && snapshot && !snapshot.available ? (
        <p className={styles.empty}>技能管理当前不可用，请检查运行时连接和诊断信息。</p>
      ) : null}
      {tab === "skills" && snapshot?.available ? (
        snapshot.skills.length > 0 ? (
          <div className={styles.list}>
            {snapshot.skills.map((skill) => (
              <article key={skill.name}>
                <button className={styles.skill_body} onClick={() => {
                  setSelectedName(skill.name);
                  void settings.loadSkillDetail(skill.name);
                }} type="button">
                  <i data-health={skill.health} />
                  <span>
                    <strong title={skill.name}>{skill.name}</strong>
                    <em>{skill.description || "暂无描述"}</em>
                    <small>{sourceLabel(skill.source)} · {qualificationLabel(skill)}</small>
                  </span>
                  <Icon name="chevron-right" size={15} />
                </button>
                <label className={styles.switch} title={skill.enabled ? "已启用" : "已禁用"}>
                  <input
                    checked={skill.enabled}
                    disabled={settings.pending_skill_name !== null}
                    onChange={(event) => void settings.setSkillEnabled(skill.name, event.target.checked)}
                    type="checkbox"
                  />
                  <span />
                  <b>{skill.enabled ? "已启用" : "已禁用"}</b>
                </label>
              </article>
            ))}
          </div>
        ) : <p className={styles.empty}>所选范围内没有可用技能。</p>
      ) : null}
      {tab === "diagnostics" && snapshot ? <SkillDiagnostics diagnostics={snapshot.diagnostics} /> : null}
      <SettingsMessages />
    </SettingsPageContainer>
  );
});

function SkillDetail(props: Readonly<{
  detail: SkillDetailSnapshot | null;
  diagnostics: readonly SkillDiagnosticSnapshot[];
  loading: boolean;
  skill: SkillSummarySnapshot;
  on_back: () => void;
}>) {
  const skill = props.detail?.skill ?? props.skill;
  return (
    <SettingsPageContainer
      back_label="返回技能列表"
      on_back={props.on_back}
      title={skill.name}
    >
      <dl className={styles.detail_list}>
        <div className={styles.description_row}><dt>描述</dt><dd>{skill.description || "暂无描述"}</dd></div>
        <div><dt>状态</dt><dd>{healthLabel(skill.health)} · {skill.enabled ? "已启用" : "已禁用"}</dd></div>
        <div><dt>生效来源</dt><dd>{sourceLabel(skill.source)}</dd></div>
        <div><dt>调用资格</dt><dd>{qualificationLabel(skill)}</dd></div>
      </dl>
      {!skill.enabled && <p className={styles.warning}>所有同名来源都已按名称屏蔽。</p>}
      <section className={styles.detail_body}>
        <h4>技能正文</h4>
        {props.loading ? <p className={styles.body_placeholder}>正在读取技能正文…</p> : null}
        {!props.loading && props.detail?.body ? (
          <div className={styles.body_content}><MarkdownContent text={props.detail.body} /></div>
        ) : null}
        {!props.loading && !props.detail?.body ? (
          <p className={styles.body_placeholder}>当前没有可确定展示的技能正文。</p>
        ) : null}
      </section>
      {props.diagnostics.length > 0 && <div className={styles.detail_diagnostics}>
        <h4>相关诊断与覆盖关系</h4>
        <SkillDiagnostics diagnostics={props.diagnostics} />
      </div>}
      <p className={styles.detail_note}>这里展示当前来源中的技能正文；已有会话仍使用创建时冻结的技能数据。</p>
      <SettingsMessages />
    </SettingsPageContainer>
  );
}

function SkillDiagnostics({ diagnostics }: Readonly<{ diagnostics: readonly SkillDiagnosticSnapshot[] }>) {
  if (diagnostics.length === 0) return <p className={styles.empty}>没有技能诊断。</p>;
  return <div className={styles.diagnostics}>{diagnostics.map((item, index) => (
    <article data-severity={item.severity} key={`${item.code}-${index}`}>
      <strong>{item.severity === "error" ? "错误" : "警告"}{item.skill_name ? ` · ${item.skill_name}` : ""}</strong>
      <p title={item.detail}>{diagnosticMessage(item.code)}</p>
      <small>{item.source ? sourceLabel(item.source) : "未知来源"} · 技术代码：{item.code}</small>
    </article>
  ))}</div>;
}

function sourceLabel(source: SkillSourceSnapshot): string {
  return {
    workspace_ez_assistant: "工作区 .ez-assistant",
    workspace_agents: "工作区 .agents",
    user_ez_assistant: "用户目录 .ez-assistant",
    user_agents: "用户目录 .agents",
  }[source];
}

function qualificationLabel(skill: SkillSummarySnapshot): string {
  const values = [skill.user_invocable ? "用户可用" : "", skill.model_invocable ? "智能体可用" : ""].filter(Boolean);
  return values.join(" · ") || "不可调用";
}

function healthLabel(health: SkillHealthSnapshot): string {
  return { ready: "正常", disabled: "已禁用", conflict: "存在冲突", unavailable: "不可用" }[health];
}

function diagnosticMessage(code: string): string {
  return ({
    root_unreadable: "无法读取该技能根目录。",
    scan_incomplete: "技能来源目录未能完整扫描，本次结果不会用于新会话。",
    candidate_limit_exceeded: "候选技能数量超过产品上限。",
    missing_definition: "技能来源目录缺少可读的 SKILL.md。",
    definition_too_large: "SKILL.md 超过允许大小。",
    frontmatter_too_large: "技能元数据超过允许大小。",
    invalid_frontmatter: "技能元数据格式无效。",
    missing_required_field: "技能缺少必填名称或描述。",
    invalid_name: "技能名称格式无效。",
    invalid_description: "技能描述为空或过长。",
    optional_field_defaulted: "可选字段无效，已采用缺省值。",
    unknown_field: "技能包含当前版本不识别的字段。",
    special_file: "技能包包含不支持的特殊文件。",
    same_source_conflict: "相同来源层级存在多个同名技能，无法确定生效项。",
    disabled_by_name: "该名称已禁用，所有同名来源均被屏蔽。",
    shadowed: "该候选项已被更高优先级的同名来源覆盖。",
    not_invocable: "该技能未向用户或智能体开放调用。",
    catalog_limit_exceeded: "该技能超出单个会话技能数量上限。",
    legacy_catalog_unavailable: "历史会话没有可恢复的技能信息。",
  } as Record<string, string>)[code] ?? "技能无法正常加载，请查看技术代码。";
}
