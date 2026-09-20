import { cpSync, mkdtempSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkAdminAssets } from '../dist/admin-assets.js';

// 只装配已分别构建的产物，不写回源码、不触碰既有交付目录，也不执行安装/数据库升级。
const center = fileURLToPath(new URL('../', import.meta.url));
const admin = join(dirname(center), 'enterprise-admin');
const metadata = JSON.parse(readFileSync(join(center, 'package.json'), 'utf8'));
const adminMetadata = JSON.parse(readFileSync(join(admin, 'package.json'), 'utf8'));
const builtMetadata = JSON.parse(readFileSync(join(center, 'dist/package-info.json'), 'utf8'));
if (metadata.version !== adminMetadata.version || metadata.version !== builtMetadata.version) {
  throw new Error('前后端及后端构建版本不一致，请分别重新构建');
}
await checkAdminAssets(join(admin, 'dist/admin'));
const directory = mkdtempSync(join(tmpdir(), 'ez-enterprise-center-'));
cpSync(join(center, 'dist'), join(directory, 'dist'), { recursive: true });
cpSync(join(admin, 'dist/admin'), join(directory, 'public/admin'), { recursive: true });
for (const file of ['package.json', 'package-lock.json', 'README.md', '.env.example']) {
  cpSync(join(center, file), join(directory, file));
}
await checkAdminAssets(join(directory, 'public/admin'));
process.stdout.write(JSON.stringify({ directory, version: metadata.version }) + '\n');
