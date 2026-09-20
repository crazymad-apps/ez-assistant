import { action, computed, makeObservable, observable, runInAction } from 'mobx';
import { api, failure } from '../request/api';
import type { IdentityUser } from '../request/openapi';
import { userView } from '../model';
import type { UserView, UserDraft } from '../model';
import { clearTokenCookie, readTokenCookie, writeTokenCookie } from './token-cookie';

/**
 * 身份与凭证权威（单例，组件直接 import）：当前用户、内存 Token、退出确认和写入锁。
 * 页面列表、筛选和加载态是路由页面临时态，由页面级读取器（PagedList）承担，不进本 Store。
 */
export class AdminStore {
  user?: IdentityUser;
  token = '';
  notice = '';
  logoutState: 'none' | 'pending' | 'failed' = 'none';
  busy = false;
  restoreState: 'none' | 'pending' | 'failed' = 'none';
  private identityRequest?: AbortController;
  private loginAttempt?: object;
  constructor() {
    makeObservable<this, 'clear' | 'write'>(this, {
      user: observable.ref,
      token: observable,
      notice: observable,
      logoutState: observable,
      busy: observable,
      restoreState: observable,
      currentUser: computed,
      clear: action,
      write: action,
      dispose: action.bound,
      invalidateIdentity: action.bound,
      login: action.bound,
      logout: action.bound,
      verifyIdentity: action.bound,
      restoreIdentity: action.bound,
      saveUser: action.bound,
      toggle: action.bound,
      resetPassword: action.bound,
      changePassword: action.bound,
    });
  }
  get currentUser() {
    return this.user ? userView(this.user) : undefined;
  }
  private clear(notice = '', forgetToken = true) {
    if (forgetToken) clearTokenCookie(this.token);
    this.loginAttempt = undefined;
    this.identityRequest?.abort();
    this.user = undefined;
    this.token = '';
    this.logoutState = 'none';
    this.notice = notice;
    this.restoreState = 'none';
  }
  dispose() {
    // 应用销毁只释放内存和在途请求，不等同于登出，保留刷新恢复所需 Cookie。
    this.clear('', false);
  }
  private valid(token: string) {
    return this.token === token && this.logoutState === 'none';
  }
  invalidateIdentity(token: string, notice: string) {
    // 迟到的旧 Token 失败不能清空另一身份；页面投影随路由守卫卸载，不由本方法逐个清理。
    if (token && this.valid(token)) this.clear(notice);
  }
  async login(username: string, password: string): Promise<string | undefined> {
    if (this.loginAttempt) return '正在登录，请稍候。';
    const attempt = {};
    this.loginAttempt = attempt;
    try {
      const result = await api.login(username, password);
      if (this.loginAttempt !== attempt) {
        // 页面已卸载；不恢复身份，并尽力撤销迟到的登录凭据。
        try {
          await api.logout(result.token);
        } catch {
          /* 不再展示或复用该凭据。 */
        }
        return '登录页面已关闭，请重新登录。';
      }
      if (!result.user.enabled || (!result.user.is_super_admin && result.user.role !== 'admin')) {
        // 普通用户后台误入只撤销本次新 Token，不影响其 Runtime 的其他登录。
        try {
          await api.logout(result.token);
        } catch {
          return '此账号无后台权限；本次登录退出尚未确认，请联系管理员。';
        }
        return '此账号无管理后台权限，请通过 Runtime 登录。';
      }
      runInAction(() => {
        this.clear();
        this.token = result.token;
        this.user = result.user;
        writeTokenCookie(result.token);
      });
    } catch (error) {
      return failure(error).text;
    } finally {
      if (this.loginAttempt === attempt) this.loginAttempt = undefined;
    }
  }
  restoreIdentity() {
    if (this.token) return;
    this.token = readTokenCookie();
    return this.verifyIdentity();
  }
  async verifyIdentity() {
    const token = this.token;
    if (!token || !this.valid(token) || this.busy) return;
    this.identityRequest?.abort();
    const request = new AbortController();
    this.identityRequest = request;
    if (!this.user) this.restoreState = 'pending';
    try {
      const identity = await api.me(token, request.signal);
      runInAction(() => {
        if (this.valid(token) && !request.signal.aborted && this.identityRequest === request) {
          if (!identity.user.enabled || (!identity.user.is_super_admin && identity.user.role !== 'admin'))
            this.clear('管理权限已变更，请通过 Runtime 登录。');
          else {
            this.user = identity.user;
            this.restoreState = 'none';
          }
        }
      });
    } catch {
      // 身份失效由统一拦截器处理；恢复失败保留凭据与原路由，只允许重试身份查询。
      runInAction(() => {
        if (this.valid(token) && !request.signal.aborted && this.identityRequest === request && !this.user)
          this.restoreState = 'failed';
      });
    }
  }
  async logout() {
    if (!this.token || this.busy || this.logoutState === 'pending') return;
    const token = this.token;
    this.logoutState = 'pending';
    try {
      await api.logout(token);
      runInAction(() => {
        if (this.token === token) this.clear();
      });
    } catch {
      // 退出失败遮蔽后台并允许以同一 Token 重试；页面数据由遮蔽层卸载隐藏。
      runInAction(() => {
        if (this.token === token) this.logoutState = 'failed';
      });
    }
  }
  /** 写入不自动重试；成功与后续读取失败分开呈现，不让保存按钮重新发起已完成的操作。 */
  private async write(work: (token: string) => Promise<unknown>, selfInvalidates = false): Promise<string | undefined> {
    if (this.busy) return '操作正在提交，请稍候。';
    const token = this.token;
    if (!token || !this.valid(token)) return '登录已失效，请重新登录。';
    this.busy = true;
    this.identityRequest?.abort();
    try {
      await work(token);
      if (!this.valid(token)) return '当前身份已变更，请重新核对操作结果。';
      if (selfInvalidates) {
        runInAction(() => this.clear('账号或密码已变更，请重新登录。'));
        return;
      }
      // 列表投影在页面本地；写入成功由调用方回调页面刷新，Store 不感知列表。
    } catch (error) {
      return failure(error, true).text;
    } finally {
      runInAction(() => {
        this.busy = false;
      });
    }
  }
  saveUser(draft: UserDraft, original?: UserView) {
    if (original)
      return this.write(
        async (token) => {
          const updated = await api.update(token, original.id, { display_name: draft.displayName, role: draft.role });
          runInAction(() => {
            if (this.valid(token) && this.user?.id === updated.id) this.user = updated;
          });
        },
        original.id === this.user?.id && original.role !== draft.role,
      );
    return this.write((token) =>
      api.create(token, {
        username: draft.username,
        display_name: draft.displayName,
        role: draft.role,
        enabled: true,
        password: draft.password ?? '',
      }),
    );
  }
  toggle(user: UserView) {
    return this.write((token) => api.update(token, user.id, { enabled: !user.enabled }), user.id === this.user?.id);
  }
  resetPassword(user: UserView, password: string) {
    return this.write((token) => api.reset(token, user.id, password), user.id === this.user?.id);
  }
  changePassword(oldPassword: string, newPassword: string) {
    return this.write((token) => api.password(token, oldPassword, newPassword), true);
  }
}

// 单例导出，组件直接 import 使用，不经 props 或路由参数透传。
export default new AdminStore();
