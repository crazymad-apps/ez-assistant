// 仅保存后台 Token，不保存用户投影、密码或 LLM key；后端仍只认显式 Bearer。
// Cookie 不隔离端口，因此名称包含端口，避免同主机多个 Center 开发实例互相覆盖。
function name() {
  return `ez_admin_token_${location.port || 'default'}`;
}

export function readTokenCookie(): string {
  if (typeof document === 'undefined') return '';
  const entry = document.cookie.split(';').find((part) => part.trim().startsWith(`${name()}=`));
  if (!entry) return '';
  try {
    return decodeURIComponent(entry.trim().slice(name().length + 1));
  } catch {
    return '';
  }
}

export function writeTokenCookie(token: string) {
  if (typeof document === 'undefined') return;
  // 会话 Cookie 保持刷新登录态，不额外承诺浏览器关闭后的长期记住登录。
  document.cookie = `${name()}=${encodeURIComponent(token)}; Path=/admin/; SameSite=Strict${location.protocol === 'https:' ? '; Secure' : ''}${token ? '' : '; Max-Age=0'}`;
}

export function clearTokenCookie(token: string) {
  // 旧标签页失效不能删除另一标签页后来登录的凭据。
  if (token && readTokenCookie() === token) writeTokenCookie('');
}
