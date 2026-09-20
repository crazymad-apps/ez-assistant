import { isAxiosError } from 'axios';
import { configure } from 'mobx';
import { client } from './request/openapi/client.gen';
import { failure } from './request/api';
import adminStore from './stores/AdminStore';

/** 渲染前统一装配请求环境；凭据权威为应用 Store 单例，不复制到 client defaults。 */
export function bootstrap() {
  configure({ enforceActions: 'always' });
  const store = adminStore;
  client.setConfig({ baseURL: '', timeout: 10000, throwOnError: true });

  const requestInterceptor = client.instance.interceptors.request.use((config) => {
    // 显式头绑定调用发起时的身份，也用于撤销未进入后台的临时登录；不能被当前 Token 覆盖。
    if (!config.headers.has('Authorization') && store.token) {
      config.headers.set('Authorization', `Bearer ${store.token}`);
    }
    return config;
  });

  const responseInterceptor = client.instance.interceptors.response.use(
    (response) => response,
    (error: unknown) => {
      if (isAxiosError(error) && !error.config?.signal?.aborted) {
        const result = failure(error);
        const authorization = error.config?.headers.get('Authorization');
        if (
          (result.code === 'TOKEN_INVALID' || result.code === 'ADMIN_REQUIRED') &&
          typeof authorization === 'string' &&
          authorization.startsWith('Bearer ')
        ) {
          // 迟到的旧 Token 失败不能清空另一身份；断网及普通业务错误不触发退出。
          store.invalidateIdentity(authorization.slice(7), result.text);
        }
      }
      // 让原调用方区分读取失败和写入结果不明，不在全局重复弹 Toast 或重试请求。
      return Promise.reject(error);
    },
  );

  // 同步进入恢复态，再发起本人查询；路由首次渲染不会把尚未恢复的身份当成未登录。
  void store.restoreIdentity();
  return {
    store,
    dispose: () => {
      client.instance.interceptors.request.eject(requestInterceptor);
      client.instance.interceptors.response.eject(responseInterceptor);
      store.dispose();
    },
  };
}
