import { copyFileSync, cpSync, mkdirSync } from 'node:fs';
// 构建产物只由脚本复制版本化 SQL；不打包结构快照。
cpSync('src/database/migrations', 'dist/database/migrations', { recursive: true });
copyFileSync('package.json', 'dist/package-info.json');

mkdirSync('dist/resources', { recursive: true });
copyFileSync('../../packages/assistant-protocol/resources/model-templates.json', 'dist/resources/model-templates.json');
