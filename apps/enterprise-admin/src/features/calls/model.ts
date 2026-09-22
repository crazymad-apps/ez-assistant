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
  disabled: '未采集',
  pending: '保存中',
  complete: '完整',
  partial: '部分',
  failed: '保存或校验失败',
  expired: '已过期',
  missing: '文件缺失',
};
