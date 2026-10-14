import type { CallDetails, CallSnapshot } from '../../request/openapi';

export const outcomeLabels: Record<CallDetails['outcome'], string> = {
  in_progress: '转发中',
  forwarded: '转发完成',
  rejected: '准入拒绝',
  upstream_error: '上游错误',
  interrupted: '已中断',
  unknown: '结果未知',
};
export const snapshotLabels: Record<CallSnapshot['state'], string> = {
  disabled: '未留存',
  not_received: '未收到',
  pending: '保存中',
  complete: '完整',
  partial: '部分',
  failed: '保存或校验失败',
  expired: '已过期',
  missing: '文件缺失',
};

export const usageLabels: Record<CallDetails['usage_state'], string> = {
  pending: '统计中',
  available: '完整',
  partial: '部分可用',
  unavailable: '无用量数据',
};

export const usageReasonLabels: Record<string, string> = {
  historical: '升级前无用量数据',
  not_forwarded: '未发送到上游',
  not_reported: '上游未返回用量',
  interrupted: '流式响应中断',
  incomplete_usage: '上游字段不完整',
  invalid_usage: '上游用量格式或数值异常',
  unsupported_content: '响应类型不支持解析',
  unsupported_encoding: '响应使用了不支持的压缩编码',
  parser_limit: '用量解析超过大小上限',
  parser_budget: '用量解析缓冲不足',
  process_restarted: '中心重启前未完成记录',
};

export const recordingLabels = { off: '关闭', anomalies: '仅异常与未确认', all: '全部调用' };
export const observationLabels = {
  pending: '观察中',
  normal: '响应正常结束',
  anomalous: '响应异常',
  unconfirmed: '响应未确认',
};
export const evidenceReasonLabels: Record<string, string> = {
  policy_off: '当时未启用正文采集',
  normal_not_retained: '按策略未保留正常正文',
  missing_terminal: '未观察到可靠结束信号',
  invalid_format: '响应格式损坏',
  invalid_utf8: '非 UTF-8 字节',
  response_failed: '模型响应失败',
  response_incomplete: '模型未完整结束',
  response_cancelled: '上游报告响应取消',
  response_error: '上游返回错误',
  terminal_conflict: '响应终态冲突',
  response_identity_conflict: '响应身份冲突',
  invalid_response_structure: '响应结构异常',
  content_after_terminal: '终态后仍有输出',
  connect_timeout: '上游连接超时',
  upstream_idle: '上游等待超时',
  downstream_idle: '下游交付超时',
  downstream_closed: '下游连接关闭',
  center_shutdown: '中心关闭',
  identity_revoked: '主体权限已撤销',
  bridge_configuration_changed: '桥接配置已失效',
  cancelled: '调用中断，原因未确认',
  not_forwarded: '未发送到上游',
  http_error: 'HTTP 错误',
  upstream_connection: '上游连接异常',
  parser_limit: '观察解析达到大小上限',
  parser_budget: '观察解析缓冲不足',
  incomplete_frame: '响应尾部事件不完整',
  unsupported_encoding: '响应编码未解析',
  unsupported_content: '响应类型未解析',
  size_limit: '采集达到大小上限',
  memory_limit: '采集缓冲不足',
  interrupted: '响应交付中断',
  process_restarted: '中心重启前未完成',
  recovered_unknown: '恢复的文件，完整性未确认',
  file_missing: '快照文件缺失',
  integrity_mismatch: '快照校验不一致',
  snapshot_write_failed: '快照保存失败',
};

export function snapshotLabel(snapshot: CallSnapshot): string {
  return snapshot.reason === 'normal_not_retained' ? '正常未留存' : snapshotLabels[snapshot.state];
}

export const sourceLabels = {
  upstream_http: '上游 HTTP 原文',
  websocket_bridge: 'WS 桥接后的响应',
  websocket_compact: 'WS 压缩转换结果',
};
