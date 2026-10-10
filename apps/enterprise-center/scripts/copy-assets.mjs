import { copyFileSync, cpSync, mkdirSync, existsSync } from 'node:fs';
// 构建产物只由脚本复制版本化 SQL；不打包结构快照。
cpSync('src/database/migrations', 'dist/database/migrations', { recursive: true });
copyFileSync('package.json', 'dist/package-info.json');

mkdirSync('dist/resources', { recursive: true });
copyFileSync('../../packages/assistant-protocol/resources/model-templates.json', 'dist/resources/model-templates.json');

// 正式公钥由发布准备提供；没有公钥时中心仍可运行，但上传保持关闭。
const updatePublicKey = '../../packages/assistant-protocol/resources/desktop-update.pub';
if (existsSync(updatePublicKey)) copyFileSync(updatePublicKey, 'dist/resources/desktop-update.pub');
