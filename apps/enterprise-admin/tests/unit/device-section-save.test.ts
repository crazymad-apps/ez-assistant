import { describe, expect, it } from 'vitest';
import type { HostDeviceState } from '../../src/request/openapi';
import { sectionChange } from '../../src/features/devices/model';

const base: HostDeviceState = { name: '原名称', status: 'registered', enabled: true, users: [] };
const grant = { user_id: 2, desktop_view: true, desktop_control: false };

describe('设备详情独立保存', () => {
  it('保存名称保留中心已保存的权限，不夹带用户草稿', () => {
    const current = { ...base, users: [grant] };
    const change = sectionChange(base, { ...base, name: ' 新名称 ', users: [] }, current, 'info');
    expect(change).toEqual({ expected: current, desired: { ...current, name: '新名称' } });
  });

  it('保存用户保留中心已保存的名称，不夹带名称草稿', () => {
    const current = { ...base, name: '已保存名称' };
    const change = sectionChange(base, { ...base, name: '未保存名称', users: [grant] }, current, 'users');
    expect(change).toEqual({ expected: current, desired: { ...current, users: [grant] } });
  });

  it('同区内容被他人修改时保留原 expected，由中心拒绝覆盖', () => {
    const nameChange = sectionChange(base, { ...base, name: '本地名称' }, { ...base, name: '他人名称' }, 'info');
    const usersChange = sectionChange(
      base,
      { ...base, users: [grant] },
      { ...base, users: [{ ...grant, user_id: 3 }] },
      'users',
    );
    expect(nameChange.expected).toEqual(base);
    expect(usersChange.expected).toEqual(base);
  });

  it('注销或停用竞争不能借分区合并绕过冲突', () => {
    for (const section of ['info', 'users'] as const) {
      for (const current of [
        { ...base, enabled: false },
        { ...base, status: 'revoked' as const },
      ]) {
        expect(sectionChange(base, { ...base, name: '新名称', users: [grant] }, current, section).expected).toEqual(
          base,
        );
      }
    }
  });
});
