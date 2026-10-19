import { useEffect, useRef, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Button } from '../../../components/Button';
import { Dialog } from '../../../components/Dialog';
import { Icon } from '../../../components/Icon';
import { SelectionPopover } from '../../../components/SelectionPopover';
import { useInputMethodGuard } from '../../../components/InputMethodGuard';
import type { ApplicationConnectionStore } from '../ApplicationConnectionStore';
import { loginOptionsChanged } from '../loginOptions';
import styles from './index.module.scss';

/** 窄管理表单独立于业务 RootStore；编辑与影响确认始终使用同一个弹窗。 */
export const LoginOptionsDialog = observer(function LoginOptionsDialog({
  connection,
}: Readonly<{ connection: ApplicationConnectionStore }>) {
  const state = connection.login_options;
  const [mode_open, setModeOpen] = useState(false);
  const title_ref = useRef<HTMLHeadingElement>(null);
  const fields_ref = useRef<HTMLDivElement>(null);
  const back_ref = useRef<HTMLButtonElement>(null);
  const address_ref = useRef<HTMLInputElement>(null);
  const input_method = useInputMethodGuard();
  const phase = state?.phase;
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      if (phase === 'edit') fields_ref.current?.querySelector<HTMLButtonElement>('button')?.focus();
      if (phase === 'confirm') back_ref.current?.focus();
    });
    return () => cancelAnimationFrame(frame);
  }, [phase]);
  useEffect(() => {
    if (state?.address_error) address_ref.current?.focus();
  }, [state?.address_error]);
  if (!state) return null;
  const baseline = state.baseline;
  const changed = baseline ? loginOptionsChanged(baseline, state.draft) : false;
  const busy = connection.login_options_busy;
  const blocked = connection.updates.downloading || connection.updates.installing;
  const confirm = phase === 'confirm';
  const editable = phase === 'edit';
  let progress = '正在读取登录选项…';
  if (phase === 'saving') progress = '正在保存…';
  if (phase === 'checking_save') progress = '正在核对保存结果…';
  if (phase === 'restarting') progress = '正在重启…';
  if (phase === 'checking_applied') progress = '正在重新连接并核对生效状态…';
  let primary = '保存并重启';
  if (!changed && baseline?.restart_required) primary = '重启并应用已保存配置';
  if (confirm) primary = changed ? '确认保存并重启' : '确认重启';
  const modeLabel = (mode: string | undefined) => (mode === 'enterprise' ? '企业模式' : '个人模式');
  return (
    <Dialog
      aria_labelledby="login-options-title"
      backdrop_class_name={styles.backdrop}
      dialog_class_name={styles.dialog}
      initial_focus_ref={title_ref}
      dismissible={!busy && !mode_open}
      on_close={() => connection.closeLoginOptions()}
    >
      <header>
        <h2 id="login-options-title" ref={title_ref} tabIndex={-1}>
          {confirm ? '确认应用登录选项' : '登录选项'}
        </h2>
        <Button
          iconOnly
          variant="text"
          aria-label="关闭登录选项"
          disabled={busy}
          onClick={() => connection.closeLoginOptions()}
        >
          <Icon name="x" size={16} />
        </Button>
      </header>
      <div className={styles.body}>
        {(phase === 'loading' || busy) && <p role="status">{progress}</p>}
        {editable && baseline && (
          <>
            {baseline.restart_required && (
              <div className={styles.notice} role="status">
                <strong>已保存，待重启</strong>
                <dl className={styles.comparison}>
                  <div>
                    <dt>运行中</dt>
                    <dd>{modeLabel(baseline.current_mode)}</dd>
                    <dd>{baseline.current_mode === 'enterprise' ? baseline.current_center_url : '未使用企业中心'}</dd>
                  </div>
                  <div>
                    <dt>已保存</dt>
                    <dd>{modeLabel(baseline.mode)}</dd>
                    <dd>{baseline.mode === 'enterprise' ? baseline.center_url : '未使用企业中心'}</dd>
                  </div>
                </dl>
              </div>
            )}
            <form
              id="login-options-form"
              onSubmit={(event) => {
                event.preventDefault();
                connection.confirmLoginOptions();
              }}
              onCompositionStart={input_method.onCompositionStart}
              onCompositionEnd={input_method.onCompositionEnd}
              onKeyUp={input_method.onKeyUp}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && input_method.shouldIgnoreKeyDown(event)) event.preventDefault();
              }}
            >
              <div className={styles.fields} ref={fields_ref}>
                <label>使用模式</label>
                <SelectionPopover
                  aria_label="使用模式"
                  trigger_variant="field"
                  open={mode_open}
                  on_open_change={setModeOpen}
                  selected={state.draft.mode}
                  on_select={(mode) => connection.editLoginOptions({ mode })}
                  options={[
                    { value: 'personal', label: '个人模式' },
                    { value: 'enterprise', label: '企业模式' },
                  ]}
                />
                {state.draft.mode === 'enterprise' && (
                  <>
                    <label htmlFor="login-center-address">企业中心地址</label>
                    <input
                      id="login-center-address"
                      ref={address_ref}
                      value={state.draft.center_url}
                      placeholder="输入企业中心地址，如 https://center.example.com"
                      autoComplete="url"
                      aria-invalid={Boolean(state.address_error)}
                      aria-describedby={state.address_error ? 'login-center-error' : undefined}
                      onChange={(event) => connection.editLoginOptions({ center_url: event.target.value })}
                    />
                    {state.address_error && (
                      <p id="login-center-error" role="alert" className={styles.error}>
                        {state.address_error}
                      </p>
                    )}
                  </>
                )}
              </div>
            </form>
          </>
        )}
        {confirm && baseline && (
          <div className={styles.confirmation}>
            <dl className={styles.changes}>
              {state.draft.mode !== baseline.current_mode && (
                <>
                  <dt>使用模式</dt>
                  <dd>
                    {modeLabel(baseline.current_mode)} → {modeLabel(state.draft.mode)}
                  </dd>
                </>
              )}
              {state.draft.mode === 'enterprise' && state.draft.center_url.trim() !== baseline.current_center_url && (
                <>
                  <dt>企业中心地址</dt>
                  <dd>
                    {baseline.current_center_url || '未使用企业中心'} → {state.draft.center_url.trim()}
                  </dd>
                </>
              )}
            </dl>
            {state.draft.mode === 'personal' && baseline.center_url && <p>企业中心地址保留。</p>}
            <p>将重启这台电脑的 Runtime，中断全部用户任务并断开所有客户端。任务不会自动续跑。当前任务数量未知。</p>
            {changed && baseline.configuration.remote_enabled && (
              <p>保存后将关闭非本地访问，现有远程连接会断开；重启后需在工作区访问设置中重新开启。</p>
            )}
            {changed && baseline.center_id && <p>已有设备登记和用户数据保留，切换后使用所选中心的登记与授权。</p>}
            {baseline.restart_required && <p>本次重启也会应用其他已保存的待重启配置。</p>}
          </div>
        )}
        {phase === 'issue' && baseline && (
          <dl className={styles.changes}>
            <dt>本次选择</dt>
            <dd>
              {modeLabel(state.draft.mode)}
              {state.draft.mode === 'enterprise' && <span> · {state.draft.center_url}</span>}
            </dd>
          </dl>
        )}
        {!busy && state.message && (
          <p role="alert" className={styles.error}>
            {state.message}
          </p>
        )}
        {blocked && <p role="status">更新正在进行，完成后可应用登录选项。</p>}
      </div>
      <footer>
        <Button
          ref={back_ref}
          disabled={busy}
          onClick={() => (confirm ? connection.editLoginOptions() : connection.closeLoginOptions())}
        >
          {confirm ? '返回修改' : '取消'}
        </Button>
        {(editable || confirm) && (
          <Button
            variant="primary"
            disabled={blocked || busy || (!changed && !baseline?.restart_required)}
            type={editable ? 'submit' : 'button'}
            form={editable ? 'login-options-form' : undefined}
            onClick={confirm ? () => void connection.applyLoginOptions() : undefined}
          >
            {primary}
          </Button>
        )}
        {phase === 'issue' && state.recovery === 'reload' && (
          <Button onClick={() => void connection.reloadLoginOptions()}>重新载入</Button>
        )}
        {phase === 'issue' && state.recovery === 'restart' && (
          <Button variant="primary" disabled={blocked} onClick={() => connection.confirmLoginOptions()}>
            重启并应用已保存配置
          </Button>
        )}
        {phase === 'issue' && (state.recovery === 'check_save' || state.recovery === 'check_applied') && (
          <Button onClick={() => void connection.checkLoginOptions()}>重新检查</Button>
        )}
      </footer>
    </Dialog>
  );
});
