import type { FeedbackSummary } from '../../request/openapi';

export const statusLabels: Record<FeedbackSummary['status'], string> = {
  pending: '待处理',
  processing: '处理中',
  resolved: '已解决',
  closed: '已关闭',
};
export const kindLabels: Record<FeedbackSummary['target_kind'], string> = {
  platform: '平台',
  plugin: '插件',
  knowledge: '知识库',
};
export const sourceLabels: Record<FeedbackSummary['source'], string> = { manual: '手动', agent: 'Agent' };
export const statusOptions = Object.entries(statusLabels).map(([value, label]) => ({ value, label }));
export const kindOptions = Object.entries(kindLabels).map(([value, label]) => ({ value, label }));
export const sourceOptions = Object.entries(sourceLabels).map(([value, label]) => ({ value, label }));
