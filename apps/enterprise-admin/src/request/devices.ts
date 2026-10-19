import * as sdk from './openapi/sdk.gen';
import type { ListHostDevicesData, ManageHostDevice } from './openapi';

const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}` },
  signal,
  throwOnError: true as const,
});
export const devicesApi = {
  list: async (token: string, query: ListHostDevicesData['query'], signal: AbortSignal) =>
    (await sdk.listHostDevices({ ...options(token, signal), query })).data,
  get: async (token: string, id: string, signal: AbortSignal) =>
    (await sdk.getHostDevice({ ...options(token, signal), path: { id } })).data,
  save: async (token: string, id: string, body: ManageHostDevice) =>
    (await sdk.manageHostDevice({ ...options(token), path: { id }, body })).data,
};
