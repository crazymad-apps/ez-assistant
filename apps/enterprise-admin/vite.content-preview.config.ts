import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { readFile, realpath } from 'node:fs/promises';
import { resolve, join } from 'node:path';

/** 仅本地开发入口；正式构建不加载此中间件，不挂载整个源目录。 */
export default defineConfig({
  root: 'content-preview',
  define: { 'import.meta.env.VITE_SITE_CONTENT_PREVIEW': JSON.stringify('true') },
  plugins: [
    react(),
    {
      name: 'local-site-content',
      configureServer(server) {
        const root = process.env.SITE_CONTENT_PREVIEW_DIR;
        if (!root) throw new Error('请设置 SITE_CONTENT_PREVIEW_DIR 为静态内容输出目录');
        server.middlewares.use(async (request, response, next) => {
          const path = request.url?.split('?')[0] ?? '';
          if (path !== '/__content' && !path.startsWith('/__assets/')) return next();
          const host = request.headers.host ?? '';
          if (
            !/^(127\.0\.0\.1|localhost)(:\d+)?$/.test(host) ||
            (request.headers.origin && request.headers.origin !== `http://${host}`)
          ) {
            response.statusCode = 403;
            response.end();
            return;
          }
          try {
            const contentFile = join(resolve(root), 'content.json');
            if ((await realpath(contentFile)) !== contentFile) throw new Error();
            const bytes = await readFile(contentFile);
            const content = JSON.parse(bytes.toString()) as { assets: { id: string; type: string; sha256: string }[] };
            response.setHeader('Cache-Control', 'no-store');
            response.setHeader('X-Content-Type-Options', 'nosniff');
            if (path === '/__content') {
              response.setHeader('Content-Type', 'application/json');
              response.end(bytes);
              return;
            }
            const id = path.slice('/__assets/'.length);
            const asset = content.assets.find(
              (item) => `${item.sha256}.${item.type.slice(6)}` === id && /^[0-9a-f]{64}\.(png|jpeg|webp)$/.test(id),
            );
            if (!asset) {
              response.statusCode = 404;
              response.end();
              return;
            }
            response.setHeader('Content-Type', asset.type);
            const imageFile = join(resolve(root), 'assets', id);
            if ((await realpath(imageFile)) !== imageFile) throw new Error();
            response.end(await readFile(imageFile));
          } catch {
            response.statusCode = 503;
            response.end('预览内容不可用');
          }
        });
      },
    },
  ],
  server: { host: '127.0.0.1', port: 7313, strictPort: true },
});
