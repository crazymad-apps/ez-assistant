import { rmSync } from 'node:fs';

// 仅清理本包的编译输出，避免已删除的源码/资源继续混入新制品。
rmSync(new URL('../dist/', import.meta.url), { recursive: true, force: true });
