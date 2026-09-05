import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState, type Ref } from "react";
import { InlineIconButton } from "../../../components/InlineIconButton";
import { Button } from "../../../components/Button";
import { Icon } from "../../../components/Icon";
import { openExternalHttpUrl } from "../../../native-bridge/openExternalUrl";
import type { BrowserController } from "../BrowserController";
import styles from "./index.module.scss";

export const ResourceBrowser = observer(function ResourceBrowser(props: Readonly<{
  controller: BrowserController;
  active: boolean;
  viewport_ref: Ref<HTMLDivElement>;
}>) {
  const browser = props.controller;
  const [address, setAddress] = useState(browser.url);
  const [editing, setEditing] = useState(false);
  const address_input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (props.active && !browser.url) address_input.current?.focus();
  }, [props.active, browser.url]);
  useEffect(() => { if (!editing) setAddress(browser.url); }, [browser.url, editing]);
  useEffect(() => {
    if (!props.active || !browser.native_id || browser.error) return;
    // 仅活动标签查询原生 URL，同步 SPA/history 路由，不读取或注入页面 DOM。
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      await browser.refreshUrl();
      if (!cancelled) timer = setTimeout(() => void poll(), 750);
    }
    void poll();
    return () => { cancelled = true; clearTimeout(timer); };
  }, [props.active, browser, browser.native_id, browser.error]);

  function openUrl(url: string) {
    if (url) void openExternalHttpUrl(url).catch((failure: unknown) => browser.reportNotice(failure));
  }
  function openInSystem() { openUrl(browser.url); }
  const notice = browser.notice ?? (browser.load_delayed ? "网页加载较慢，可继续浏览或重试。" : null);
  const notice_url = browser.notice ? browser.notice_url : browser.url;

  return (
    <div className={styles.browser}>
      <form className={styles.toolbar} onSubmit={(event) => {
        event.preventDefault();
        if (browser.navigate(address)) setEditing(false);
      }}>
        <InlineIconButton disabled={!browser.native_id} icon="chevron-left" label="后退" onClick={() => browser.perform("back")} />
        <InlineIconButton disabled={!browser.native_id} icon="chevron-right" label="前进" onClick={() => browser.perform("forward")} />
        <InlineIconButton disabled={!browser.native_id} icon={browser.loading ? "stop" : "refresh"} label={browser.loading ? "停止加载" : "刷新网页"} onClick={() => browser.perform(browser.loading ? "stop" : "reload")} />
        <input
          aria-label="网页地址" autoCapitalize="none" autoComplete="off" autoCorrect="off"
          className={styles.address} onBlur={() => setEditing(false)} onChange={(event) => setAddress(event.target.value)}
          onFocus={(event) => { setEditing(true); event.currentTarget.select(); }}
          placeholder="输入网址" ref={address_input} spellCheck={false} type="text" value={address}
        />
        <InlineIconButton disabled={!browser.url} icon="external-link" label="在系统浏览器中打开" onClick={openInSystem} />
        {browser.loading && <div className={styles.progress} role="progressbar" aria-label="网页加载中" />}
      </form>
      {notice && <div className={styles.notice} role="status">
        <span>{notice}</span>
        {!browser.notice && browser.load_delayed && <Button onClick={() => browser.perform("reload")} size="small" variant="text">重试</Button>}
        {notice_url && <Button onClick={() => openUrl(notice_url)} size="small" variant="text">系统浏览器打开</Button>}
        <InlineIconButton icon="x" label="关闭提示" onClick={() => browser.dismissNotice()} />
      </div>}
      <div aria-busy={browser.loading} className={styles.viewport} ref={props.viewport_ref}>
        {browser.error ? <div className={styles.state} role="alert">
          <p>{browser.error}</p>
          <div className={styles.actions}>
            <Button onClick={() => browser.navigate(browser.url || address)} size="small" variant="text">重试</Button>
            {browser.url && <Button onClick={openInSystem} size="small" variant="text">系统浏览器打开</Button>}
          </div>
        </div> : !browser.url && <div className={styles.state}>
          <Icon name="globe" size={32} />
          <p>输入网址，开始浏览</p>
        </div>}
      </div>
    </div>
  );
});
