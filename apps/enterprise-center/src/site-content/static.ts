import { constants } from 'node:fs';
import { open, realpath } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import type { FastifyInstance } from 'fastify';
import { limits } from './validation.js';

/** 目录由部署者配置，只有固定 JSON 与哈希图片可读；不遍历仓库、不跟随链接、不依赖数据库。 */
export function registerSiteContent(server: FastifyInstance, directory?: string) {
  const root = directory ? resolve(directory) : undefined;
  server.get('/site-content/*', async (request, reply) => {
    const name = (request.params as { '*': string })['*'];
    const asset = /^assets\/[0-9a-f]{64}\.(png|jpeg|webp)$/.exec(name);
    if (!root || (name !== 'content.json' && !asset)) return reply.code(404).send();
    const file = join(root, name);
    try {
      if (await realpath(file) !== file) return reply.code(404).send();
      const handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stat = await handle.stat(), cap = asset ? limits.image : limits.projection;
        if (!stat.isFile() || stat.size > cap) return reply.code(503).send();
        // 一次读固定句柄；目录发布原子替换 JSON 时，不会把两个文件内容拼接。
        const bytes = Buffer.alloc(stat.size + 1);
        let length = 0;
        while (length < bytes.length) {
          const read = await handle.read(bytes, length, bytes.length - length, length);
          if (!read.bytesRead) break;
          length += read.bytesRead;
        }
        if (length !== stat.size) return reply.code(503).send();
        reply.header('Cache-Control', asset ? 'public, max-age=31536000, immutable' : 'no-cache');
        reply.header('X-Content-Type-Options', 'nosniff');
        return reply.type(asset ? `image/${asset[1]}` : 'application/json').send(bytes.subarray(0, length));
      } finally { await handle.close(); }
    } catch (error) {
      const missing = (error as NodeJS.ErrnoException).code === 'ENOENT';
      return reply.code(missing ? 404 : 503).send();
    }
  });
}
