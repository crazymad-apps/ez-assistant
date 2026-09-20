import type { CenterConfig } from './config.js';
import { createHttpApp } from './app.js';
import { checkDatabase } from './database/check.js';
import { createPool } from './database/pool.js';
import { upgradeDatabase } from './database/upgrade.js';
import { createDataSource } from './database/source.js';
import { checkAdminAssets } from './admin-assets.js';

/** 维护池完成升级即关闭，随后唯一业务 DataSource 接管；HTTP 关闭后再释放业务池。 */
export async function startService(config: CenterConfig, adminRoot?: string) {
  if (adminRoot) await checkAdminAssets(adminRoot);
  const pool = createPool(config.database);
  try {
    if (config.autoUpgrade) await upgradeDatabase(pool);
    await checkDatabase(pool);
  } finally {
    await pool.end();
  }
  const source = createDataSource(config.database);
  let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
  try {
    await source.initialize();
    app = await createHttpApp({ source, origin: config.origin }, adminRoot);
    await app.listen(config.port, config.host);
    const runningApp = app;
    let closing: Promise<void> | undefined;
    return {
      address: await app.getUrl(),
      close: () => {
        closing ??= (async () => {
          try {
            await runningApp.close();
          } finally {
            if (source.isInitialized) await source.destroy();
          }
        })();
        return closing;
      },
    };
  } catch (error) {
    try {
      await app?.close();
    } finally {
      if (source.isInitialized) await source.destroy();
    }
    throw error;
  }
}
