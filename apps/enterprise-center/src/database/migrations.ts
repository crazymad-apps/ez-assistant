import { readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { CenterError } from '../errors.js';

/** 只维护 SQL 文件，不再维护第二份 manifest；只读准入与框架使用同一目录及名称。 */
export function upgradeFiles() {
  const directory = fileURLToPath(new URL('./migrations/', import.meta.url));
  const files = readdirSync(directory).sort((a, b) => a.localeCompare(b, 'en', { numeric: true }));
  if (!files.length || files.some(file => !/^\d+-[a-z0-9-]+\.sql$/.test(file))) {
    throw new CenterError('INVALID_UPGRADE_FILES', '数据库升级文件缺失或命名无效。');
  }
  return { directory, names: files.map(file => file.slice(0, -4)) };
}
