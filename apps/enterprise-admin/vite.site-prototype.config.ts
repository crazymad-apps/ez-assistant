import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

/** 官网原型沿用现有前端依赖，独立入口和产物不进入正式 Center 装配。 */
export default defineConfig({
  root: 'prototype',
  plugins: [react()],
  build: {
    outDir: '../dist/site-prototype',
    emptyOutDir: true,
    rollupOptions: { input: fileURLToPath(new URL('./prototype/site/index.html', import.meta.url)) },
  },
});
