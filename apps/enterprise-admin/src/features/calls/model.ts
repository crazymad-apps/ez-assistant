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
  disabled: '当时未启用正文采集',
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
