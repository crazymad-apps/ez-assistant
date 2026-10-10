import { expect, it, vi } from 'vitest';
import { modelApi } from '../../src/request/models';
import * as sdk from '../../src/request/openapi/sdk.gen';

vi.mock('../../src/request/openapi/sdk.gen', () => ({ saveModelSettings: vi.fn() }));

it('地址和默认模型独立提交，携带当前身份且不提交另一项的旧草稿', async () => {
  vi.mocked(sdk.saveModelSettings).mockResolvedValue({ data: {} } as Awaited<ReturnType<typeof sdk.saveModelSettings>>);
  await modelApi.setPublicOrigin('current-token', 'http://localhost:7320');
  expect(sdk.saveModelSettings).toHaveBeenLastCalledWith(
    expect.objectContaining({
      headers: expect.objectContaining({ Authorization: 'Bearer current-token' }),
      body: { public_origin: 'http://localhost:7320' },
    }),
  );
  await modelApi.setDefault('current-token', null);
  expect(sdk.saveModelSettings).toHaveBeenLastCalledWith(expect.objectContaining({ body: { default_model: null } }));
  await modelApi.setPublicOrigin('current-token', null);
  expect(sdk.saveModelSettings).toHaveBeenLastCalledWith(expect.objectContaining({ body: { public_origin: null } }));
});
