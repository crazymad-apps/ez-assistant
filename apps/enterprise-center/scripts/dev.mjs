import { existsSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

// Node watch 每次重启先完成整个编译，再加载服务，避免监听 dist 时读到半套输出。
// .env 在被重启的子进程加载，因此编辑配置也能生效；外部环境变量仍优先。
if (existsSync('.env')) process.loadEnvFile('.env');
await import('./clean.mjs');
const build = spawnSync(process.execPath, ['node_modules/typescript/bin/tsc'], { stdio: 'inherit' });
if (build.error || build.status !== 0) process.exit(build.status || 1);
await import('./copy-assets.mjs');
await import('../dist/main.js');
