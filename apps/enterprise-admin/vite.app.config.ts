import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  root: 'src',
  base: '/admin/',
  plugins: [react()],
  build: { outDir: '../dist/admin', emptyOutDir: true, manifest: true },
  // 开发时也从浏览器同源请求 /api；生产由 Center 同源提供静态资源。
  server: { proxy: { '/api': { target: 'http://127.0.0.1:7320', changeOrigin: false } } },
});
