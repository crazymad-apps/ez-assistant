import { lstat, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { CenterError } from './errors.js';

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/** 复用 Vite 构建清单校验入口、共享和延迟加载资源；启动失败发生在任何数据库操作之前。 */
export async function checkAdminAssets(root: string): Promise<void> {
  const fail = () => {
    throw new CenterError('ADMIN_ASSETS_INVALID', '管理后台资源缺失或不完整，请重新装配 Center 制品。');
  };
  try {
    // 制品资源不能通过软链逃离静态根目录，也不能将目录误当成资源文件。
    async function file(path: string) {
      let current = root;
      const parts = path.split('/');
      for (const [index, part] of parts.entries()) {
        if (!part || part === '.' || part === '..' || part.includes('\\')) fail();
        current = join(current, part);
        const stat = await lstat(current);
        if (
          stat.isSymbolicLink() ||
          (index === parts.length - 1 ? !stat.isFile() || stat.size === 0 : !stat.isDirectory())
        )
          fail();
      }
      await readFile(current);
    }
    const stat = await lstat(root);
    if (!stat.isDirectory() || stat.isSymbolicLink()) fail();
    await file('index.html');
    await file('.vite/manifest.json');
    const html = await readFile(join(root, 'index.html'), 'utf8');
    const manifest: unknown = JSON.parse(await readFile(join(root, '.vite/manifest.json'), 'utf8'));
    if (!record(manifest)) return fail();
    const entry = manifest['index.html'];
    if (
      !record(entry) ||
      entry.isEntry !== true ||
      typeof entry.file !== 'string' ||
      !html.includes(`/admin/${entry.file}`)
    )
      return fail();
    for (const chunk of Object.values(manifest)) {
      if (!record(chunk) || typeof chunk.file !== 'string') return fail();
      const resources: unknown[] = [chunk.file];
      for (const key of ['css', 'assets']) {
        const values = chunk[key];
        if (values !== undefined) {
          if (!Array.isArray(values)) return fail();
          resources.push(...values);
        }
      }
      for (const key of ['imports', 'dynamicImports']) {
        const values = chunk[key];
        if (
          values !== undefined &&
          (!Array.isArray(values) ||
            values.some((name: unknown) => typeof name !== 'string' || !Object.hasOwn(manifest, name)))
        )
          fail();
      }
      for (const resource of resources) {
        if (typeof resource !== 'string' || !/^assets\/[a-zA-Z0-9_./-]+$/.test(resource)) return fail();
        await file(resource);
      }
    }
  } catch {
    fail();
  }
}
