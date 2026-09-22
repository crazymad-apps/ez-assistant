const controls = {
  llm_key_invalid: [401, '模型调用凭据无效。'],
  managed_model_configuration_stale: [409, '中心默认模型已变化，请刷新配置。'],
  managed_model_unavailable: [409, '中心默认模型不可用。'],
  llm_request_invalid: [400, '模型请求格式无效。'],
  llm_request_too_large: [413, '模型请求超过正文限制。'],
  llm_record_unavailable: [503, '调用记录暂时不可用。'],
  llm_proxy_unavailable: [503, '模型代理暂时不可用。'],
} as const;

export class ControlError extends Error {
  readonly status: number;

  constructor(readonly code: keyof typeof controls) {
    super(controls[code][1]);
    this.status = controls[code][0];
  }
}
