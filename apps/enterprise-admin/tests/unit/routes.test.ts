import { createElement } from 'react';
import { renderToString } from 'react-dom/server';
import { createMemoryRouter, RouterProvider } from 'react-router';
import { runInAction } from 'mobx';
import { beforeEach, describe, expect, it } from 'vitest';
import { adminRoutes } from '../../src/app/routes';
import adminStore from '../../src/stores/AdminStore';

// 组件直接读取 Store 单例；每个用例前清空身份，避免用例间状态泄漏。
beforeEach(() => {
  adminStore.dispose();
});

describe('正式页面路由', () => {
  it.each(['pending', 'failed'] as const)('身份恢复 %s 时不展示登录页或管理数据，保持当前路由', (state) => {
    runInAction(() => {
      adminStore.restoreState = state;
    });
    for (const path of ['/login', '/users', '/audit/1']) {
      const router = createMemoryRouter(adminRoutes(), { initialEntries: [path] });
      try {
        const html = renderToString(createElement(RouterProvider, { router }));
        expect(html).toContain(state === 'pending' ? '正在恢复登录' : '暂时无法恢复登录');
        expect(html).not.toMatch(/管理员登录|成员列表|操作记录/);
        expect(router.state.location.pathname).toBe(path);
      } finally {
        router.dispose();
      }
    }
  });
  it('身份守卫不渲染未登录用户的后台，登录页为独立路由', () => {
    for (const path of ['/users', '/audit', '/audit/1']) {
      const router = createMemoryRouter(adminRoutes(), { initialEntries: [path] });
      try {
        expect(renderToString(createElement(RouterProvider, { router }))).not.toMatch(/成员列表|操作记录|请求关联 ID/);
      } finally {
        router.dispose();
      }
    }
    const router = createMemoryRouter(adminRoutes(), { initialEntries: ['/login'] });
    try {
      expect(renderToString(createElement(RouterProvider, { router }))).toContain('管理员登录');
    } finally {
      router.dispose();
    }
  });
  it('退出未确认时所有路由隐藏业务页面', () => {
    runInAction(() => {
      adminStore.logoutState = 'failed';
    });
    const router = createMemoryRouter(adminRoutes(), { initialEntries: ['/users'] });
    try {
      const html = renderToString(createElement(RouterProvider, { router }));
      expect(html).toContain('退出尚未确认');
      expect(html).not.toContain('成员列表');
    } finally {
      router.dispose();
    }
  });
});
