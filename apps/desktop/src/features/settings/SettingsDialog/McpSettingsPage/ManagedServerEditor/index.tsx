import { observer } from 'mobx-react-lite';
import { useEffect, useState } from 'react';
import type { McpServerSnapshot } from '@ez-assistant/protocol';
import { useRootStore } from '../../../../../stores/RootStoreContext';
import { SessionActionDialog } from '../../../../sessions/SessionActionDialog';
import { PluginSettingField } from '../../PluginSettingField';
import { SettingsPageContainer } from '../../SettingsPageContainer';
import { SettingsMessages } from '../../SettingsMessages';
import { SecretFields, type SecretRow } from '../ConnectionFields';
import type { ServerEditorProps } from '../ServerEditor';
import { priorityLabel, sourceLabel } from '../draft';
import styles from '../index.module.scss';
import shared from '../../index.module.scss';

export const ManagedServerEditor = observer(function ManagedServerEditor(
  props: ServerEditorProps & Readonly<{ server: McpServerSnapshot }>,
) {
  const settings = useRootStore().settings.mcp;
  // 只在本页自己提交参数后接纳新修订；后台刷新不能替换用户已经查看的草稿身份。
  const [server, setServer] = useState(props.server);
  const managed = server.managed!;
  const [enabled, setEnabled] = useState(managed.configured_enabled);
  const [rows, setRows] = useState<SecretRow[]>(() => {
    const states = server.transport === 'stdio' ? managed.environment : managed.headers;
    return Object.keys(states)
      .filter((name) => !managed.parameter_bindings[name])
      .map((name) => ({ id: name, name, change: { mode: 'keep' }, existing: true }));
  });
  const [dirty, setDirty] = useState(false);
  const [parameter_dirty, setParameterDirty] = useState<ReadonlySet<string>>(new Set());
  const [discard, setDiscard] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const unsaved = dirty || parameter_dirty.size > 0;
  const busy = settings.pending_action !== null || settings.testing;
  const unavailable = !managed.source_available || settings.stale;
  const states = server.transport === 'stdio' ? managed.environment : managed.headers;
  const runtime_label = {
    connected: '已连接',
    connected_without_tools: '已连接，无工具',
    disabled: '已禁用',
    unavailable: '尚未连接',
  }[server.runtime_state];
  const bound = new Set(Object.values(managed.parameter_bindings).flat());
  const parameters = managed.parameters.filter((field) => bound.has(field.name));
  useEffect(() => {
    props.onDirtyChange(unsaved);
  }, [unsaved, props.onDirtyChange]);
  useEffect(
    () => () => {
      settings.cancelTest();
      props.onDirtyChange(false);
    },
    [settings, props.onDirtyChange],
  );

  function leave() {
    if (unsaved) setDiscard(true);
    else props.onDone();
  }

  function draft() {
    const names = rows.map((row) => row.name.trim());
    if (names.some((name) => !name || managed.parameter_bindings[name]) || new Set(names).size !== names.length) {
      setError('名称不能为空、重复或覆盖绑定参数。');
      return null;
    }
    const changes = Object.fromEntries(rows.map((row) => [row.name.trim(), row.change]));
    return {
      target: { source: server.source, server_key: server.server_key },
      expected_definition_digest: managed.definition_digest,
      expected_revision: managed.revision,
      environment: server.transport === 'stdio' ? changes : {},
      headers: server.transport === 'streamable_http' ? changes : {},
    };
  }

  async function save() {
    const changes = draft();
    if (!changes) return;
    const { expected_revision, ...patch } = changes;
    if (await settings.mutate(expected_revision, { type: 'patch_managed', payload: { ...patch, enabled } }))
      props.onDone();
  }

  async function reset(name: string) {
    if (unsaved) return;
    const target = { source: server.source, server_key: server.server_key };
    if (
      await settings.mutate(managed.revision, {
        type: 'reset_managed',
        payload: {
          target,
          expected_definition_digest: managed.definition_digest,
          enabled: false,
          environment: server.transport === 'stdio' ? [name] : [],
          headers: server.transport === 'streamable_http' ? [name] : [],
        },
      })
    )
      props.onDone();
  }

  async function saveParameter(name: string, value: string | null): Promise<boolean> {
    settings.cancelTest();
    const current = await settings.saveParameter(server, name, value);
    if (!current || current.managed?.definition_digest !== managed.definition_digest) return false;
    setServer(current);
    return true;
  }

  return (
    <SettingsPageContainer title={server.display_name} on_back={leave} back_label={props.back_label ?? '返回 MCP 列表'}>
      <div className={styles.editor}>
        <fieldset disabled={busy || unavailable || parameter_dirty.size > 0}>
          <legend>
            {sourceLabel(server)} · {server.server_key}
          </legend>
          <p>
            {server.transport === 'stdio' ? '本地 stdio' : 'Streamable HTTP'} · {server.target_summary} ·{' '}
            {runtime_label}
            {server.needs_refresh && ' · 配置待刷新'}
          </p>
          {priorityLabel(server) && <p role="status">{priorityLabel(server)}；当前配置保留，可继续编辑。</p>}
          {!managed.source_enabled && (
            <p className={styles.warning}>所属插件已停用，服务不会启动；这里保留服务自身的启停设置。</p>
          )}
          {!managed.configuration_ready && <p className={styles.warning}>配置尚不完整，请检查必要参数或来源诊断。</p>}
          <label className={styles.checkbox}>
            <input
              aria-label="启用此服务"
              type="checkbox"
              checked={enabled}
              onChange={(event) => {
                settings.cancelTest();
                setEnabled(event.target.checked);
                setDirty(true);
              }}
            />
            启用此服务
          </label>
          <SecretFields
            label={server.transport === 'stdio' ? '环境变量' : '请求头'}
            rows={rows}
            onChange={(values) => {
              settings.cancelTest();
              setRows(values);
              setDirty(true);
              setError(null);
            }}
            managed={{ states, canReset: !unsaved, onReset: (name) => void reset(name) }}
          />
        </fieldset>
        {parameters.length > 0 && (
          <fieldset disabled={busy || unavailable}>
            <legend>绑定插件参数</legend>
            <p>参数单独保存，并影响共用该参数的服务。请先保存或放弃其他草稿。</p>
            {Object.entries(managed.parameter_bindings).map(([key, names]) => (
              <p key={key}>
                {key} → {names.join('、')}
              </p>
            ))}
            {parameters.map((field) => (
              <PluginSettingField
                key={field.name}
                label={field.label}
                secret={field.secret}
                is_set={field.is_set}
                busy={busy || dirty || (parameter_dirty.size > 0 && !parameter_dirty.has(field.name))}
                onDirtyChange={(changed) =>
                  setParameterDirty((previous) => {
                    const next = new Set(previous);
                    if (changed) next.add(field.name);
                    else next.delete(field.name);
                    return next;
                  })
                }
                onSave={(value) => saveParameter(field.name, value)}
              />
            ))}
          </fieldset>
        )}
        {unavailable && <p className={styles.error}>来源不可用或列表已过期，请重新读取后再修改。</p>}
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <SettingsMessages messages={settings} />
        {settings.configuration_conflict && (
          <div className={styles.warning}>
            <p>来源或配置已经改变，未覆盖新内容。重新载入会放弃当前草稿。</p>
            <button
              type="button"
              disabled={busy}
              onClick={async () => {
                await settings.load();
                if (!settings.stale) props.onDone();
              }}
            >
              重新载入
            </button>
          </div>
        )}
        <div aria-live="polite" className={styles.test_result}>
          {settings.testing && <p>正在连接并读取工具目录…</p>}
          {settings.test_result && (
            <p>
              连接测试{settings.test_result.outcome === 'success' ? '成功' : '未完成'} ·{' '}
              {settings.test_result.elapsed_ms} ms · {settings.test_result.tool_count} 个工具
              {settings.test_result.diagnostic && ` · ${settings.test_result.diagnostic.message}`}
            </p>
          )}
        </div>
        <p>启动定义由来源维护。保存覆盖后需刷新；测试不保存、不启用服务，也不执行搜索。</p>
        <div className={styles.actions}>
          <button
            disabled={
              busy || unavailable || parameter_dirty.size > 0 || !managed.source_enabled || !managed.configuration_ready
            }
            type="button"
            onClick={() => {
              const changes = draft();
              if (changes) void settings.test(changes);
            }}
          >
            测试连接
          </button>
          {settings.testing && (
            <button type="button" onClick={() => settings.cancelTest()}>
              取消测试
            </button>
          )}
          <button
            disabled={busy || unavailable || unsaved}
            type="button"
            onClick={async () => {
              const current = await settings.refresh(server, props.revision);
              if (current?.managed?.definition_digest === managed.definition_digest) setServer(current);
            }}
          >
            刷新此服务
          </button>
          <span />
          <button disabled={settings.pending_action !== null} type="button" onClick={leave}>
            取消
          </button>
          <button
            className={shared.primary_button}
            disabled={busy || unavailable || parameter_dirty.size > 0}
            onClick={() => void save()}
            type="button"
          >
            保存
          </button>
        </div>
      </div>
      {discard && (
        <SessionActionDialog
          title="放弃未保存的修改？"
          confirm_label="放弃修改"
          is_danger
          is_pending={false}
          on_cancel={() => setDiscard(false)}
          on_confirm={props.onDone}
        >
          <p>未保存的字段和本次输入的新凭据将被丢弃。</p>
        </SessionActionDialog>
      )}
    </SettingsPageContainer>
  );
});
