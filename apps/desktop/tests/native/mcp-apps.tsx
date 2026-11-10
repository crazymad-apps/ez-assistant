import { useMemo, useState } from 'react';
import { AppLinkEndpoint } from '../../src/features/mcp-apps/appLinks';
import { createRoot } from 'react-dom/client';
import { McpAppView, type AppPageSource } from '../../src/features/mcp-apps';

declare global {
  interface Window {
    __TAURI_INTERNALS__: { invoke: (command: string, payload: Record<string, unknown>) => Promise<unknown> };
    __EZ_F03_MAIN_FRAME_PROBE__?: boolean;
    __EZ_F03_ALL_FRAME_PROBE__?: boolean;
  }
}

async function probe(): Promise<void> {
  // 子 frame 隔离不能以禁用整个 WebView 的 IPC 为代价；先取得主 frame 的真实原生回执。
  const main_frame_ready =
    window.__EZ_F03_MAIN_FRAME_PROBE__ === true &&
    window.__EZ_F03_ALL_FRAME_PROBE__ === true &&
    (await window.__TAURI_INTERNALS__.invoke('probe_ping', {})) === true;
  const source = (await fetch(new URL('./resource.json', location.href)).then((response) =>
    response.json(),
  )) as AppPageSource;
  const host = document.getElementById('root')!;
  const root = createRoot(host);
  let settled = false;
  let isolation_passed = false;
  let snapshot_passed = false;
  let links_passed = false;
  let retired = false;
  window.addEventListener('message', async (event) => {
    const frame = host.querySelector('iframe');
    const facts = event.data?.params?.data?.f03Proof;
    const snapshot = event.data?.params?.data?.f03SnapshotProof;
    const link = event.data?.params?.data?.f03LinkProof;
    if (settled || event.source !== frame?.contentWindow || event.origin !== 'null') return;
    if (facts)
      isolation_passed =
        main_frame_ready &&
        facts.parentReadable === false &&
        facts.cookieExposed === false &&
        facts.tauriAvailable === false &&
        facts.mainFrameScript === false &&
        facts.allFrameScript === true;
    if (snapshot) snapshot_passed ||= snapshot.saved === true && snapshot.revision === 1 && snapshot.count === 2;
    if (link) links_passed = link.count === 12 && link.linked === true;
    if (!isolation_passed || !snapshot_passed || !links_passed) return;
    settled = true;
    root.unmount();
    const passed = isolation_passed && snapshot_passed && links_passed && retired;
    await window.__TAURI_INTERNALS__.invoke('probe_finish', {
      passed,
      summary: passed
        ? '正式 Tauri CSP 下握手、主 frame IPC、子 frame 隔离、快照、双页面关联回传及卸载通过'
        : '原生隔离事实不符合预期：' + JSON.stringify(facts),
    });
  });
  function Views() {
    const [opened, setOpened] = useState(false);
    const peer = useMemo(
      () =>
        new AppLinkEndpoint(
          ['post-message'],
          async () => {
            throw new Error('peer cannot open');
          },
          async () => {
            throw new Error('no draft');
          },
        ),
      [],
    );
    const caller = useMemo(
      () =>
        new AppLinkEndpoint(
          ['open-app', 'post-message'],
          async (request) => {
            if (
              request.target.type !== 'plugin' ||
              request.target.name !== 'ui-fixture' ||
              request.target.entry_id !== 'counter-app'
            )
              throw new Error('wrong target');
            setOpened(true);
            return peer;
          },
          async () => {
            throw new Error('no draft');
          },
        ),
      [peer],
    );
    return (
      <>
        <McpAppView
          source={source}
          links={caller}
          context={{ displayMode: 'inline', availableDisplayModes: ['inline'], platform: 'desktop' }}
          input={{ count: 1, native_snapshot_probe: true, native_link_probe: true }}
          card={{
            revision: 0,
            snapshot: null,
            schema: 'v1',
            save: async (revision, snapshot) => {
              const count = (snapshot.value as { count: number }).count;
              if (snapshot.schema !== 'v1' || !((revision === 0 && count === 2) || (revision === 1 && count === 12)))
                throw new Error('原生快照请求不符合预期');
              return { saved: true, revision: revision + 1, snapshot };
            },
            retire: () => {
              retired = true;
              peer.retire();
            },
          }}
        />
        {opened && (
          <McpAppView
            source={source}
            links={peer}
            context={{ displayMode: 'inline', availableDisplayModes: ['inline'], platform: 'desktop' }}
            input={{ native_peer_probe: true }}
          />
        )}
      </>
    );
  }
  root.render(<Views />);
}

void probe().catch(async () => {
  await window.__TAURI_INTERNALS__.invoke('probe_finish', { passed: false, summary: '原生页面或资源加载失败' });
});
