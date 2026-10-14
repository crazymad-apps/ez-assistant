import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  root: 'src/site',
  base: '/',
  plugins: [react()],
  build: {
    outDir: '../../dist/site',
    emptyOutDir: true,
    manifest: true,
    assetsDir: 'site-assets',
    rolldownOptions: {
      output: {
        // 官网入口仅接纳字母、数字、下划线与连字符组成的资源名。
        chunkFileNames: (chunk) => `site-assets/${chunk.name.replace(/[^a-zA-Z0-9_-]/g, '-')}-[hash].js`,
      },
    },
  },
  server: { proxy: { '/api': 'http://127.0.0.1:7320', '/site-content': 'http://127.0.0.1:7320' } },
});
