export class CenterError extends Error {
  constructor(readonly code: string, message: string) { super(message); }
}

/** 底层错误可能包含 SQL、用户名或连接秘密，禁止作为日志或 HTTP 文案透传。 */
export function safeError(error: unknown): { code: string; message: string } {
  if (error instanceof CenterError) return { code: error.code, message: error.message };
  return { code: 'SERVICE_UNAVAILABLE', message: '中心服务暂时不可用，请核查运行环境。' };
}
