import type { CallDetails } from '../../request/openapi';
export const callKindLabel: Record<CallDetails['kind'], string> = {
  proxy: '模型调用',
  admin_test: '配置测试',
  context_compaction: '上下文压缩',
  input_tokens: '输入计量',
};
