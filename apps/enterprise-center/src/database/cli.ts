import { databaseConfig, databaseTarget } from '../config.js';
import { safeError } from '../errors.js';
import { createPool } from './pool.js';
import { checkDatabase } from './check.js';
import { upgradeDatabase } from './upgrade.js';

try {
  const action = process.argv[2];
  if (!['check', 'upgrade'].includes(action ?? '') || process.argv.length !== 3) throw new Error('invalid command');
  const config = databaseConfig(process.env);
  // 明确脱敏目标，命令不接受默认库或自动创建库；部署者仍须执行项目要求的写前确认。
  process.stdout.write(JSON.stringify({ action, target: databaseTarget(config) }) + '\n');
  const pool = createPool(config);
  try {
    let result;
    if (action === 'upgrade') {
      const upgrade = await upgradeDatabase(pool);
      result = { ...upgrade, ...(await checkDatabase(pool, true)) };
    } else result = await checkDatabase(pool, true);
    process.stdout.write(JSON.stringify(result) + '\n');
  } finally { await pool.end(); }
} catch (error) { process.stderr.write(JSON.stringify(safeError(error)) + '\n'); process.exitCode = 1; }
