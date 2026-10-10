import * as sdk from './openapi/sdk.gen';
import type { DesktopReleaseNotesInput, DesktopReleasePublicationInput } from './openapi';

const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}` },
  signal,
  throwOnError: true as const,
});

export const desktopReleaseApi = {
  list: async (token: string, signal: AbortSignal) => (await sdk.listDesktopReleases(options(token, signal))).data,

  get: async (token: string, version: string, signal?: AbortSignal) =>
    (await sdk.getDesktopRelease({ ...options(token, signal), path: { version } })).data,

  notes: async (token: string, version: string, body: DesktopReleaseNotesInput) =>
    (await sdk.updateDesktopReleaseNotes({ ...options(token), path: { version }, body })).data,

  publish: async (token: string, version: string, target: string, body: DesktopReleasePublicationInput) =>
    (await sdk.setDesktopReleasePublication({ ...options(token), path: { version, target }, body })).data,

  upload: async (
    token: string,
    manifest: string,
    signature: string,
    files: File[],
    signal: AbortSignal,
    progress: (percent: number) => void,
  ) =>
    (
      await sdk.uploadDesktopRelease({
        ...options(token, signal),
        body: {
          manifest: btoa(Array.from(new TextEncoder().encode(manifest), (byte) => String.fromCharCode(byte)).join('')),
          signature,
          files,
        },
        timeout: 31 * 60 * 1000,
        onUploadProgress: (event) =>
          progress(event.total ? Math.min(100, Math.round((event.loaded / event.total) * 100)) : 0),
      })
    ).data,
};
