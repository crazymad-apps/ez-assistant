import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import type { FastifyInstance } from 'fastify';
import fastifyStatic from '@fastify/static';

/** 只匹配明确页面；资源/API/后台与未知路径没有 SPA 兜底。 */
export function registerSitePages(server: FastifyInstance, root: string) {
  server.register(fastifyStatic, {
    root: join(root, 'site-assets'), prefix: '/site-assets/', decorateReply: false,
    index: false, dotfiles: 'deny', list: false, redirect: false,
    allowedPath: path => /^\/[a-zA-Z0-9_-]+\.(js|css|svg|png|webp|woff2?)$/.test(path),
  });
  for (const url of ['/', '/download', '/changelog', '/manual', '/manual/:articleId']) {
    server.get(url, async (request, reply) => {
      const article = (request.params as { articleId?: string }).articleId;
      if (article && !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(article)) return reply.code(404).send();
      try { return reply.type('text/html; charset=utf-8').send(await readFile(join(root, 'index.html'))); }
      catch { return reply.code(503).send(); }
    });
  }
}
