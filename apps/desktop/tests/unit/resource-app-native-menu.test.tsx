import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ResourceAddMenu } from '../../src/features/resource-workspace/ResourceAddMenu';
import * as native from '../../src/native-bridge/resourceMenu';
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});
it('wires the native app menu to the same picker callback and availability', async () => {
  const selected = vi.fn();
  const popup = vi.fn();
  vi.spyOn(native, 'createResourceMenu').mockImplementation(async (_workspace, _browser, _terminal, _desktop, app) => ({
    popup: async (...args) => {
      popup(...args);
      app?.();
    },
    dispose: async () => {},
  }));
  render(
    <ResourceAddMenu
      workspace_available={false}
      app_available
      on_app={selected}
      on_workspace={vi.fn()}
      on_browser={vi.fn()}
      on_error={vi.fn()}
    />,
  );
  fireEvent.click(screen.getByRole('button', { name: '新建资源标签' }));
  await waitFor(() => expect(selected).toHaveBeenCalledOnce());
  expect(popup.mock.calls[0][4]).toBe(true);
});
