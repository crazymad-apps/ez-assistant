import { Button } from '../../../components/Button';
import { observer } from 'mobx-react-lite';
import { useCallback, useEffect, useRef, useState } from 'react';
import type { ModelSelection, ProviderInstanceId, ProviderRequest } from '@ez-assistant/protocol';
import { CollapsibleSection } from '../../../components/CollapsibleSection';
import { Icon } from '../../../components/Icon';
import { ProviderIcon, inferModelProviderIcon } from '../../../components/ProviderIcon';
import { useRootStore } from '../../../stores/RootStoreContext';
import { SessionActionDialog } from '../../sessions/SessionActionDialog';
import { ModelFixedConfigPage } from './ModelFixedConfigPage';
import { ManualModelPage } from './ManualModelPage';
import { ModelSelectionControl } from './ModelSelectionControl';
import { ProviderConnectionForm } from './ProviderConnectionForm';
import { ProviderSettingsPage } from './ProviderSettingsPage';
import { SettingsMessages } from './SettingsMessages';
import { SettingsPageContainer } from './SettingsPageContainer';
import styles from './index.module.scss';

type ModelPage =
  | { type: 'home' }
  | { type: 'create' }
  | ({ type: 'provider'; auto_refresh?: boolean } & ProviderRequest)
  | ({ type: 'edit' | 'add_model' } & ProviderRequest)
  | { type: 'model'; selection: ModelSelection };

function providerPage<T extends 'provider' | 'edit' | 'add_model'>(
  type: T,
  provider: ProviderRequest,
): { type: T } & ProviderRequest {
  return { type, source: provider.source, provider_instance_id: provider.provider_instance_id };
}

export const ModelsSettingsPage = observer(function ModelsSettingsPage(
  props: Readonly<{
    onDirtyChange: (dirty: boolean) => void;
  }>,
) {
  const root = useRootStore();
  const settings = root.settings;
  const management = settings.model_settings.management;
  const enterprise = Boolean(management ?? root.local_projection.application?.model_settings.management);
  const [page, setPage] = useState<ModelPage>({ type: 'home' });
  const [dirty, setDirty] = useState(false);
  const [pending_page, setPendingPage] = useState<ModelPage | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const onDirtyChange = useCallback(
    (value: boolean) => {
      setDirty(value);
      props.onDirtyChange(value);
    },
    [props.onDirtyChange],
  );
  function navigate(next: ModelPage) {
    if (settings.pending_action) return;
    if (dirty) {
      setPendingPage(next);
      return;
    }
    settings.clearMessages();
    setPage(next);
  }
  const home = () => navigate({ type: 'home' });
  async function deleteProvider(id: ProviderInstanceId) {
    // 提交会先移除 Provider 投影并卸载详情页；完成导航由仍持有 page 的父级负责。
    const deleted = await settings.deleteProvider(id);
    if (deleted && mounted.current)
      setPage((current) => (current.type === 'provider' && current.source === 'local' && current.provider_instance_id === id
        ? { type: 'home' } : current));
  }
  const reference = page.type === 'model' ? page.selection : page.type === 'home' || page.type === 'create' ? null : page;
  const provider = settings.providers.find(
    (item) => item.provider_instance_id === reference?.provider_instance_id && item.source === reference.source,
  );
  const provider_key = provider ? `${provider.source}:${provider.provider_instance_id}` : undefined;
  let content;
  if (page.type === 'create' || (page.type === 'edit' && provider?.source === 'local')) {
    content = (
      <ProviderConnectionForm
        key={provider_key ?? 'new'}
        provider={provider ?? null}
        onDirtyChange={onDirtyChange}
        onBack={() => (provider ? navigate(providerPage('provider', provider)) : home())}
        onSaved={(saved) => {
          const auto_refresh = page.type === 'create';
          onDirtyChange(false);
          setPage({ ...providerPage('provider', saved), auto_refresh });
        }}
      />
    );
  } else if (page.type === 'provider' && provider) {
    content = (
      <ProviderSettingsPage
        key={provider_key}
        provider={provider}
        auto_refresh={page.auto_refresh}
        onBack={home}
        onDelete={() => {
          void deleteProvider(provider.provider_instance_id);
        }}
        onEdit={() => navigate(providerPage('edit', provider))}
        onAddModel={() => navigate(providerPage('add_model', provider))}
        onModel={(selection) => navigate({ type: 'model', selection })}
      />
    );
  } else if (page.type === 'add_model' && provider?.source === 'local') {
    content = (
      <ManualModelPage
        provider={provider}
        onDirtyChange={onDirtyChange}
        onDeleted={() => {
          onDirtyChange(false);
          setPage(providerPage('provider', provider));
        }}
        onBack={() => navigate(providerPage('provider', provider))}
      />
    );
  } else if (page.type === 'model' && provider) {
    content = (
      <ModelFixedConfigPage
        key={JSON.stringify(page.selection)}
        provider={provider}
        selection={page.selection}
        onDeleted={() => {
          onDirtyChange(false);
          setPage(providerPage('provider', provider));
        }}
        onDirtyChange={onDirtyChange}
        onBack={() => navigate(providerPage('provider', provider))}
      />
    );
  } else if (page.type !== 'home') {
    content = (
      <SettingsPageContainer title="服务商不可用" on_back={home}>
        <p>当前配置中没有此服务商，请返回重新选择。</p>
      </SettingsPageContainer>
    );
  } else {
    const organization = settings.providers.filter((item) => item.source === 'organization');
    const personal = settings.providers.filter((item) => item.source !== 'organization');
    function providerRows(providers: typeof settings.providers) {
      return <div className={`${styles.model_rows} ${styles.model_provider_rows}`}>
        {providers.map((item) => (
          <button
            type="button"
            aria-label={`管理服务商 ${item.connection.display_name}${item.source === 'local' ? ` ${item.connection.endpoint}` : ''}`}
            key={`${item.source}:${item.provider_instance_id}`}
            onClick={() => navigate(providerPage('provider', item))}
          >
            <span>
              <span className={styles.provider_row_identity}>
                <ProviderIcon name={inferModelProviderIcon(item.connection)} size={20} />
                <strong>{item.connection.display_name}</strong>
              </span>
              {item.source !== 'organization' && <small>{item.connection.endpoint}</small>}
            </span>
            <Icon name="chevron-right" size={16} />
          </button>
        ))}
      </div>;
    }
    const personal_rows = personal.length > 0 ? providerRows(personal) : !settings.loading && !settings.error_message && (
      <p className={styles.model_empty}>{enterprise ? '暂无个人服务商' : '暂无服务商'}</p>
    );
    content = (
      <SettingsPageContainer title="模型与服务商">
        <div className={styles.models_home}>
          <div className={styles.model_defaults}>
            <ModelSelectionControl purpose="default" read_only={enterprise} />
            <ModelSelectionControl purpose="vision" />
          </div>
          {enterprise && (
            <CollapsibleSection title="企业模型" variant="outlined" defaultOpen actions={
              <Button variant="text" aria-label="刷新配置"
                title={management?.last_success_at_ms != null
                  ? `最近获取：${new Date(management.last_success_at_ms).toLocaleString()}` : undefined}
                disabled={Boolean(settings.pending_action)} onClick={() => void settings.refreshModelSource()}>
                <Icon name="refresh" size={14} />刷新
              </Button>
            }>
              {management && (management.last_refresh_error || management.unavailable_models.length > 0) && (
                <div className={styles.model_group_messages}>
                  {management.last_refresh_error && <p role="alert">刷新失败：{management.last_refresh_error}</p>}
                  {management.unavailable_models.map(({ selection, reason }) => (
                    <p key={JSON.stringify(selection)}>{selection.model_id}（不可用） · {reason}</p>
                  ))}
                </div>
              )}
              {providerRows(organization)}
            </CollapsibleSection>
          )}
          <CollapsibleSection title={enterprise ? '个人模型' : '服务商'} variant="outlined" defaultOpen actions={
            <Button variant="primary" disabled={Boolean(settings.pending_action)} onClick={() => navigate({ type: 'create' })}>
              <Icon name="plus" size={14} />添加服务商
            </Button>
          }>{personal_rows}</CollapsibleSection>
          {settings.loading && <p role="status">正在读取模型设置…</p>}
          <SettingsMessages />
        </div>
      </SettingsPageContainer>
    );
  }
  return (
    <>
      {content}
      {pending_page && (
        <SessionActionDialog
          title="放弃未保存的修改？"
          confirm_label="放弃修改"
          is_danger
          is_pending={false}
          on_cancel={() => setPendingPage(null)}
          on_confirm={() => {
            onDirtyChange(false);
            setPage(pending_page);
            setPendingPage(null);
            settings.clearMessages();
          }}
        >
          <p>当前表单尚未保存，继续后这些修改将丢失。</p>
        </SessionActionDialog>
      )}
    </>
  );
});
