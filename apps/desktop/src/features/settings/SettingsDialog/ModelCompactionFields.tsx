import type { ModelParameters, ProviderConnection } from '@ez-assistant/protocol';
import { CollapsibleSection } from '../../../components/CollapsibleSection';
import { ModelFieldSelector } from './ModelFieldSelector';
import { support_options } from './modelSettingsValues';
import styles from './index.module.scss';

/** 草稿只表达用户选择；保存时仍由 Runtime 校验协议、能力与会话唯一机制。 */
export function ModelCompactionFields(props: Readonly<{
  parameters: ModelParameters; connection: ProviderConnection; disabled: boolean; read_only: boolean;
  on_change: (patch: Partial<ModelParameters>) => void;
}>) {
  const { parameters, connection, on_change } = props;
  const responses = connection.protocol_preference !== 'chat_completions' &&
    ['openai', 'deepseek', 'dashscope_api', 'dashscope_plan', 'moonshot'].includes(connection.provider_type);
  return <CollapsibleSection title="上下文压缩" defaultOpen>
    <div className={styles.model_form}>
      <ModelFieldSelector label="服务端压缩能力" value={parameters.server_compaction_support} options={support_options}
        disabled={props.disabled} onChange={value => on_change({ server_compaction_support: value,
          ...(value !== 'supported' ? { server_compaction_enabled: false } : {}) })} />
      <label><input type="checkbox" checked={parameters.server_compaction_enabled}
        disabled={props.disabled || (!parameters.server_compaction_enabled && (!responses || parameters.server_compaction_support !== 'supported'))}
        onChange={event => on_change({ server_compaction_enabled: event.target.checked, server_auto_compaction_enabled: false })} />
        启用服务端压缩
      </label>
      <label><input type="checkbox" checked={parameters.server_auto_compaction_enabled} disabled />服务端自行触发压缩（本版本暂不开放）</label>
      <ModelFieldSelector label="输入计量能力" value={parameters.input_token_count_support} options={support_options}
        disabled={props.disabled} onChange={value => on_change({ input_token_count_support: value })} />
    </div>
    <p>{parameters.server_compaction_enabled ? 'Agent 在达到窗口阈值或模型报告超限时调用服务端压缩；手动压缩使用相同机制。' : '自动和手动压缩均使用本地摘要。'}</p>
    <p>计量不可用时沿用现有估算，等待正常响应校准。{!responses && '服务端压缩需要 Responses 协议。'}{props.read_only && '由企业管理员管理。'}</p>
  </CollapsibleSection>;
}
