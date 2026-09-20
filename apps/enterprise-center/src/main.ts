import { loadConfig } from './config.js';
import { safeError } from './errors.js';
import { startService } from './service.js';
import { fileURLToPath } from 'node:url';

try {
  if (Number(process.versions.node.split('.')[0]) !== 24) throw new Error('Node version unsupported');
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && args[0] !== '--api-only')) throw new Error('Unknown startup option');
  const adminRoot = args[0] === '--api-only' ? undefined : fileURLToPath(new URL('../public/admin/', import.meta.url));
  const service = await startService(loadConfig(process.env), adminRoot);
  process.stdout.write(
    JSON.stringify({ event: 'center_listening', mode: adminRoot ? 'full' : 'api-only', address: service.address }) +
      '\n',
  );
  let stopping = false;
  const stop = () => {
    if (stopping) return;
    stopping = true;
    // 正常路径按 HTTP→Pool 关闭；超时仅结束当前产品进程，PG 会回滚断开的未提交事务。
    const deadline = setTimeout(() => {
      process.stderr.write('CENTER_SHUTDOWN_TIMEOUT\n');
      process.exit(1);
    }, 10000);
    void service.close().then(
      () => clearTimeout(deadline),
      (error) => {
        clearTimeout(deadline);
        process.stderr.write(JSON.stringify(safeError(error)) + '\n');
        process.exitCode = 1;
      },
    );
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
} catch (error) {
  process.stderr.write(JSON.stringify(safeError(error)) + '\n');
  process.exitCode = 1;
}
