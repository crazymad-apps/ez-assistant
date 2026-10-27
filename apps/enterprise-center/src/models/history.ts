import { createHmac } from 'node:crypto';
import type { ProviderConnection, Support, ModelConfiguration } from '../contracts/models.js';
import type { ProviderEntity } from './entity.js';

/** 与 Rust/共享 TS 的纯能力投影同义，以共享夹具约束；判断实际上游，不判断中心代理 URL。 */
export function serverHistorySupport(connection: ProviderConnection): Support {
  if (connection.provider_type === 'opencode')
    return connection.protocol_preference === 'responses' && connection.server_history_support_override !== 'unsupported' ? 'unknown' : 'unsupported';
  const responses = ['openai', 'sub2api', 'deepseek', 'dashscope_api', 'dashscope_plan', 'moonshot'].includes(connection.provider_type)
    && connection.protocol_preference !== 'chat_completions';
  if (!responses) return 'unsupported';
  if (connection.server_history_support_override !== null) return connection.server_history_support_override;
  // WebSocket 能力独立于 HTTP store；自定义路由必须显式确认。
  if (connection.responses_transport === 'websocket') {
    return connection.provider_type === 'openai' && connection.endpoint.replace(/\/+$/, '') === 'https://api.openai.com/v1'
      ? 'supported' : 'unknown';
  }
  const endpoint = connection.endpoint.replace(/\/+$/, '');
  if (connection.provider_type === 'openai' && endpoint === 'https://api.openai.com/v1') return 'supported';
  if (connection.provider_type === 'dashscope_api' && (
    /^https:\/\/(dashscope|dashscope-intl|dashscope-us|cn-hongkong\.dashscope)\.aliyuncs\.com\/compatible-mode\/v1$/.test(endpoint)
    || /^https:\/\/[a-zA-Z0-9-]+\.(cn-beijing|ap-southeast-1|us-east-1|eu-central-1|cn-hongkong|ap-northeast-1)\.maas\.aliyuncs\.com\/compatible-mode\/v1$/.test(endpoint)
  )) return 'supported';
  if (['deepseek', 'moonshot'].includes(connection.provider_type)) return 'unsupported';
  return 'unknown';
}

/** 参数对象键序不是配置变化；只用于已校验 JSON，不接受运行时句柄或循环对象。 */
export function configurationIdentity(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(configurationIdentity).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const object = value as Record<string, unknown>;
    return `{${Object.keys(object).sort().map(key => `${JSON.stringify(key)}:${configurationIdentity(object[key])}`).join(',')}}`;
  }
  return JSON.stringify(value) ?? 'null';
}

export function historyConnectionIdentity(connection: ProviderConnection, apiKey: string): string {
  return configurationIdentity([
    connection.provider_type, connection.endpoint, connection.protocol_preference,
    connection.server_history_enabled, connection.server_history_support_override, apiKey,
    // 默认 HTTP 保留旧摘要；仅新传输增加身份维度，避免升级无故失效原 HTTP 链。
    ...(connection.responses_transport === 'websocket' ? ['websocket'] : []),
  ]);
}

/** 内部周期不出站；签名投影绑定稳定主体与实际配置，同主体换 Token 或重启不换链。 */
export function historyBinding(
  provider: ProviderEntity,
  configuration: ModelConfiguration,
  centerId: string,
  userId: number,
): string | null {
  if (!(provider.connection.server_history_enabled || configuration.parameters.server_compaction_enabled) || !provider.server_history_key) return null;
  return createHmac('sha256', provider.server_history_key).update(configurationIdentity([
    centerId, userId, provider.provider_instance_id,
    historyConnectionIdentity(provider.connection, provider.api_key),
    configuration.model_id, configuration.parameters,
  ])).digest('hex');
}
