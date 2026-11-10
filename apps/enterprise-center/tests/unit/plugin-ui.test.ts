import { createWriteStream } from 'node:fs';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { ZipFile } from 'yazl';
import { afterAll, expect, it } from 'vitest';
import { parseManifest } from '../../dist/plugins/manifest.js';
import { scanArchive } from '../../dist/plugins/archive.js';

const fixture = JSON.parse(await readFile(resolve('../../packages/assistant-protocol/fixtures/plugin-ui.json'), 'utf8')) as {
  base: Record<string, unknown>;
  cases: { name: string; accept: boolean; patches: { path: string; value?: unknown; remove?: boolean }[] }[];
};
const root = await mkdtemp(resolve('../../.runtime-test/f03-ui-package-'));
afterAll(() => rm(root, { recursive: true }));

for (const item of fixture.cases) {
  it(item.name, () => {
    const manifest = structuredClone(fixture.base);
    for (const patch of item.patches) {
      const parts = patch.path.slice(1).split('/');
      let parent = manifest;
      for (const part of parts.slice(0, -1)) parent = parent[part] as Record<string, unknown>;
      const key = parts.at(-1)!;
      if (patch.remove) delete parent[key];
      else parent[key] = patch.value;
    }
    const parse = () => parseManifest(Buffer.from(JSON.stringify(manifest)));
    if (item.accept) expect(parse).not.toThrow();
    else expect(parse).toThrow();
  });
}

it('UI HTML 必须存在、为 UTF-8 且不超过 2 MiB', async () => {
  for (const html of [Buffer.from('<!doctype html><p>Generic UI</p>'), Buffer.from([0xff]), Buffer.alloc(2 * 1024 * 1024 + 1), null]) {
    const path = join(root, 'ui.zip');
    const zip = new ZipFile();
    zip.addBuffer(Buffer.from(JSON.stringify(fixture.base)), 'plugin.json');
    if (html) zip.addBuffer(html, 'resources/counter.html');
    zip.end();
    await pipeline(zip.outputStream, createWriteStream(path));
    const result = scanArchive(path, 'zip', new AbortController().signal);
    if (html && html.length < 2 * 1024 * 1024 && html[0] !== 0xff) await expect(result).resolves.toMatchObject({ format_version: 2 });
    else await expect(result).rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
  }
});
