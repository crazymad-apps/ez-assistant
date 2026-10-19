import type { HostAccessCommand, HostMode } from '@ez-assistant/protocol';
import type { LocalHostStatus } from '../../native-bridge/runtimeConnection';

export type LoginOptionsDraft = { mode: HostMode; center_url: string };
export type LoginOptionsState = {
  phase: 'loading' | 'edit' | 'confirm' | 'saving' | 'checking_save' | 'restarting' | 'checking_applied' | 'issue';
  baseline: LocalHostStatus | null;
  draft: LoginOptionsDraft;
  intended: LocalHostStatus | null;
  expected_instance: string | null;
  recovery: 'reload' | 'check_save' | 'check_applied' | 'restart' | null;
  message: string | null;
  address_error: string | null;
};

export function loginOptionsChanged(status: LocalHostStatus, draft: LoginOptionsDraft): boolean {
  // 同一 URL 也可能换了中心实例；显式保存时由 Host 核验，待重启配置不重复提交。
  return (
    (draft.mode === 'enterprise' && !status.restart_required) ||
    draft.mode !== status.mode || (draft.mode === 'enterprise' && (
      draft.center_url.trim() !== (status.center_url ?? '')
    ))
  );
}

export function centerAddressError(draft: LoginOptionsDraft): string | null {
  if (draft.mode !== 'enterprise') return null;
  const value = draft.center_url.trim();
  try {
    const url = new URL(value);
    if (
      !/^https?:\/\//i.test(value) ||
      /\s|\\/.test(value) ||
      value.split('://')[1]?.split('/')[0]?.includes('@') ||
      !url.hostname ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      value.includes('?') ||
      value.includes('#')
    )
      throw new Error();
    return null;
  } catch {
    return '请输入不含账号密码、查询参数或片段的 HTTP(S) 地址。';
  }
}

/** 个人模式不提交隐藏的地址草稿；所有访问字段从同一编辑基线保留。 */
export function loginOptionsCommand(status: LocalHostStatus, draft: LoginOptionsDraft): HostAccessCommand {
  return {
    type: 'configure',
    payload: {
      expected_revision: status.revision,
      mode: draft.mode,
      ...(draft.mode === 'enterprise' ? { center_url: draft.center_url.trim() } : {}),
      clear_center_binding: Boolean(status.center_id),
      configuration: { ...status.configuration, remote_enabled: false },
    },
  };
}

export function loginOptionsIntent(status: LocalHostStatus, draft: LoginOptionsDraft): LocalHostStatus {
  if (!loginOptionsChanged(status, draft)) return status;
  return {
    ...status,
    mode: draft.mode,
    center_url: draft.mode === 'enterprise' ? draft.center_url.trim() : status.center_url,
    center_id: undefined,
    configuration: { ...status.configuration, remote_enabled: false },
  };
}

/** 中心 ID 由 Host 解析；首次保存核对用户意图，收到权威结果后重启回读还须匹配该 ID。 */
export function matchesLoginOptions(actual: LocalHostStatus, intended: LocalHostStatus): boolean {
  const a = actual.configuration,
    b = intended.configuration;
  return (
    actual.mode === intended.mode &&
    (actual.center_url ?? '') === (intended.center_url ?? '') &&
    ((intended.mode === 'enterprise' && !intended.center_id)
      ? Boolean(actual.center_id) : (actual.center_id ?? '') === (intended.center_id ?? '')) &&
    a.remote_enabled === b.remote_enabled &&
    a.port === b.port &&
    a.scheme === b.scheme &&
    a.tls_certificate === b.tls_certificate &&
    a.tls_private_key === b.tls_private_key &&
    JSON.stringify(a.server_names) === JSON.stringify(b.server_names)
  );
}

export function loginOptionsApplied(actual: LocalHostStatus, intended: LocalHostStatus): boolean {
  return (
    matchesLoginOptions(actual, intended) &&
    !actual.restart_required &&
    actual.current_mode === intended.mode &&
    (intended.mode !== 'enterprise' || actual.current_center_url === intended.center_url)
  );
}

export function loginOptionsErrorCode(error: unknown): string | null {
  return typeof error === 'object' && error !== null && 'code' in error && typeof error.code === 'string'
    ? error.code
    : null;
}
