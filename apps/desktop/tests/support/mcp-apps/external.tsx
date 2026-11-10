import { createRoot, type Root } from 'react-dom/client';
import { McpAppView, type AppPageSource } from '../../../src/features/mcp-apps';

declare global {
  interface Window {
    f03Mount: (source: AppPageSource) => void;
    f03Unmount: () => void;
  }
}

let root: Root | null = null;
let container: HTMLDivElement | null = null;

// 仅由 Playwright 在正式 Host 文档中显式加载，产品入口不导入此测试夹具。
window.f03Unmount = () => {
  root?.unmount();
  container?.remove();
  root = null;
  container = null;
};
window.f03Mount = (source) => {
  window.f03Unmount();
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  root.render(
    <McpAppView
      source={source}
      input={{ value: 'fixture-input' }}
      result={{ content: [{ type: 'text', text: 'fixture-result' }], _meta: { private: 'fixture-ui-private' } }}
      context={{ displayMode: 'inline', availableDisplayModes: ['inline'], platform: 'web' }}
    />,
  );
};
