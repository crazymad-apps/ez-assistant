import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// 原型有独立入口和产物，不允许被默认发布流程当作正式管理页面。
export default defineConfig({
  root: 'prototype',
  plugins: [react()],
  build: { outDir: '../dist/prototype', emptyOutDir: true },
});
