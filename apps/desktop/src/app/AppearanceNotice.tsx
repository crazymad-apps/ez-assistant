import { useEffect } from 'react';
import { observer } from 'mobx-react-lite';
import { useAppMessage } from '../components/AppMessage';
import type { AppearanceStore } from '../stores/AppearanceStore';

/** 位于连接入口之外，登录前的读取或原生外观失败也有反馈。 */
export const AppearanceNotice = observer(function AppearanceNotice({ store }: { readonly store: AppearanceStore }) {
  const showMessage = useAppMessage();
  const message = store.read_error ?? (store.recovered_unknown ? '已保存的配色不可用，已使用默认配色。' : null) ?? store.native_error;
  useEffect(() => {
    // 延后到 effect 完成；StrictMode 的模拟卸载会取消第一次提示。
    const timer = message ? window.setTimeout(() => showMessage(message, 'error'), 0) : undefined;
    return () => window.clearTimeout(timer);
  }, [message, showMessage]);
  return null;
});
