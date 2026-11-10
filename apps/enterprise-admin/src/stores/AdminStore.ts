import { action, computed, makeObservable, observable, runInAction } from 'mobx';
import { callApi } from '../request/calls';
import type { RecordingSettings, ModelTestResult } from '../request/openapi';
import { modelApi } from '../request/models';
import type { ModelSelection, SaveProviderWritable as SaveProvider, SaveModelConfiguration } from '../request/openapi';
import { api, failure } from '../request/api';
import type { IdentityResponse, IdentityUser, ScopeSelection, ScopeView } from '../request/openapi';
import { userView } from '../model';
import type { UserView, UserDraft } from '../model';
import { clearTokenCookie, readTokenCookie, writeTokenCookie } from './token-cookie';

/**
 * 身份与凭证权威（单例，组件直接 import）：当前用户、内存 Token、退出确认和写入锁。
 * 页面列表、筛选和加载态是路由页面临时态，由页面级读取器（PagedList）承担，不进本 Store。
 */
export class AdminStore {
  user?: IdentityUser;
  scope?: ScopeView;
  token = '';
  notice = '';
  logoutState: 'none' | 'pending' | 'failed' = 'none';
  busy = false;
  restoreState: 'none' | 'pending' | 'failed' = 'none';
  private identityRequest?: AbortController;
  private expiryTimer?: ReturnType<typeof setTimeout>;
  private loginAttempt?: object;
  private scopeExchange?: { token: string; invalidated: boolean };

  constructor() {
    makeObservable<this, 'clear' | 'write'>(this, {
      user: observable.ref,
      scope: observable.ref,
      exchangeScope: action.bound,
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
      saveRecordingSettings: action.bound,
      testModel: action.bound,
      saveUser: action.bound,
      toggle: action.bound,
      deleteUser: action.bound,
      resetPassword: action.bound,
      changePassword: action.bound,
      saveModelProvider: action.bound,
      deleteModelProvider: action.bound,
      refreshModels: action.bound,
      saveModel: action.bound,
      resetModel: action.bound,
      setModelPublicOrigin: action.bound,
      reloadModelTemplates: action.bound,
      manage: action.bound,
    });
  }

  get currentUser() {
    return this.user ? userView(this.user) : undefined;
  }

  private clear(notice = '', forgetToken = true) {
    if (forgetToken) clearTokenCookie(this.token);
    this.loginAttempt = undefined;
    this.scopeExchange = undefined;
    clearTimeout(this.expiryTimer);
    this.identityRequest?.abort();
    this.user = undefined;
    this.scope = undefined;
    this.token = '';
    this.logoutState = 'none';
    this.busy = false;
    this.notice = notice;
    this.restoreState = 'none';
  }

  private applyIdentity(identity: IdentityResponse) {
    this.user = identity.user;
    this.scope = identity.scope;
    clearTimeout(this.expiryTimer);
    const token = this.token;
    const remaining = Date.parse(identity.expires_at) - Date.now();
    if (Number.isFinite(remaining)) {
      this.expiryTimer = setTimeout(
        () => this.invalidateIdentity(token, '登录已到期，请重新登录。'),
        Math.max(0, Math.min(remaining, 7 * 24 * 60 * 60 * 1000)),
      );
    }
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
    if (!token || !this.valid(token)) return;
    if (this.scopeExchange?.token === token) {
      // 其他请求可能先观察到换发已提交。等换发响应决定当前凭据。
      this.scopeExchange.invalidated = true;
      return;
    }
    this.clear(notice);
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
      runInAction(() => {
        this.clear();
        this.token = result.token;
        this.applyIdentity(result);
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
    if (!this.user || this.restoreState === 'failed') this.restoreState = 'pending';
    try {
      const identity = await api.me(token, request.signal);
      runInAction(() => {
        if (this.valid(token) && !request.signal.aborted && this.identityRequest === request) {
          if (!identity.user.enabled) {
            this.clear('账号已停用。');
            return;
          }
          if (!this.user && identity.scope.kind === 'unselected') {
            // 旧版本可能把首次选择前的临时凭据写进了 Cookie。
            this.clear();
            return;
          }
          this.applyIdentity(identity);
          this.restoreState = 'none';
        }
      });
    } catch {
      // 身份失效由统一拦截器处理；恢复失败保留凭据与原路由，只允许重试身份查询。
      runInAction(() => {
        if (
          this.valid(token) &&
          !request.signal.aborted &&
          this.identityRequest === request &&
          this.restoreState === 'pending'
        )
          this.restoreState = 'failed';
      });
    }
  }

  async exchangeScope(target: Omit<ScopeSelection, 'entry'>): Promise<string | undefined> {
    const token = this.token;
    if (!token || !this.valid(token) || this.busy) return '当前操作尚未结束。';
    this.identityRequest?.abort();
    this.busy = true;
    const exchange = { token, invalidated: false };
    this.scopeExchange = exchange;
    try {
      const result = await api.exchange(token, target);
      if (!this.valid(token) || this.scopeExchange !== exchange) return '登录状态已变更。';
      runInAction(() => {
        this.token = result.token;
        this.applyIdentity(result);
        writeTokenCookie(result.token);
      });
    } catch (error) {
      const problem = failure(error);
      if (this.scopeExchange !== exchange) return problem.text;
      if (problem.code === 'TOKEN_INVALID') {
        runInAction(() => this.clear(problem.text));
      } else if (exchange.invalidated || problem.code === undefined || problem.code === 'SERVICE_UNAVAILABLE') {
        // 可能已提交换发。核验原 Token 前遮蔽旧工作区，保留原凭据用于恢复。
        runInAction(() => {
          this.scopeExchange = undefined;
          this.restoreState = 'pending';
          this.busy = false;
        });
        await this.verifyIdentity();
      }
      return problem.text;
    } finally {
      runInAction(() => {
        if (this.scopeExchange === exchange) {
          this.scopeExchange = undefined;
          this.busy = false;
        }
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
  private async write(
    work: (token: string) => Promise<unknown>,
    selfInvalidates = false,
    onFailure?: (code: unknown) => void,
  ): Promise<string | undefined> {
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
      const problem = failure(error, true);
      onFailure?.(problem.code);
      return problem.text;
    } finally {
      runInAction(() => {
        this.busy = false;
      });
    }
  }

  /** 新管理页面共用身份快照与单次写入门；页面仍持有自己的草稿和结果。 */
  manage<T>(work: (token: string) => Promise<T>, completed: (result: T) => void, failed?: (code: unknown) => void) {
    return this.write(
      async (token) => {
        const result = await work(token);
        if (this.valid(token)) completed(result);
      },
      false,
      failed,
    );
  }

  saveRecordingSettings(settings: RecordingSettings) {
    return this.write((token) => callApi.save(token, settings));
  }

  testModel(selection: ModelSelection, signal: AbortSignal, completed: (result: ModelTestResult) => void) {
    return this.write(async (token) => {
      const result = await callApi.test(token, selection, signal);
      if (this.valid(token) && !signal.aborted) completed(result);
    });
  }

  saveModelProvider(input: SaveProvider, id?: string, saved?: (id: string) => void) {
    return this.write(async (token) => {
      const provider = await modelApi.saveProvider(token, input, id);
      if (this.valid(token)) saved?.(provider.provider_instance_id);
    });
  }

  deleteModelProvider(id: string) {
    return this.write((token) => modelApi.remove(token, id));
  }

  refreshModels(id: string) {
    return this.write((token) => modelApi.refresh(token, id));
  }

  saveModel(input: SaveModelConfiguration) {
    return this.write((token) => modelApi.save(token, input));
  }

  resetModel(selection: ModelSelection) {
    return this.write((token) => modelApi.reset(token, selection));
  }

  setModelPublicOrigin(origin: string | null) {
    return this.write((token) => modelApi.setPublicOrigin(token, origin));
  }

  reloadModelTemplates() {
    return this.write((token) => modelApi.reload(token));
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

  deleteUser(user: UserView) {
    return this.write((token) => api.remove(token, user.id), user.id === this.user?.id);
  }

  resetPassword(user: UserView, password: string) {
    return this.write((token) => api.reset(token, user.id, password));
  }

  changePassword(oldPassword: string, newPassword: string) {
    return this.write((token) => api.password(token, oldPassword, newPassword));
  }
}

// 单例导出，组件直接 import 使用，不经 props 或路由参数透传。
export default new AdminStore();
