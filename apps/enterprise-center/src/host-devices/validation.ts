import { deviceStatuses } from '../contracts/host-devices.js';
import type { ApplyHostDevice, DeviceStatus, HostDeviceState } from '../contracts/host-devices.js';
import { fields, invalid } from '../validation.js';

export function deviceId(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value))
    invalid();
  return value;
}

export function secret(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{64}$/.test(value)) invalid();
  return value;
}

function label(value: unknown, max: number): string {
  if (
    typeof value !== 'string' ||
    !value.trim() ||
    value !== value.trim() ||
    value.length > max ||
    [...value].some((character) => character.charCodeAt(0) < 32 || (character.charCodeAt(0) >= 127 && character.charCodeAt(0) <= 159))
  )
    invalid();
  return value;
}

export function deviceStatus(value: unknown): DeviceStatus {
  if (!deviceStatuses.includes(value as DeviceStatus)) invalid();
  return value as DeviceStatus;
}

export function application(value: unknown): ApplyHostDevice {
  const v = fields(value, ['center_id', 'id', 'installation_id', 'secret', 'name', 'platform', 'host_version']);
  return {
    center_id: deviceId(v.center_id),
    id: deviceId(v.id),
    installation_id: deviceId(v.installation_id),
    secret: secret(v.secret),
    name: label(v.name, 80),
    platform: label(v.platform, 32),
    host_version: label(v.host_version, 64),
  };
}

export function state(value: unknown): HostDeviceState {
  const v = fields(value, ['status', 'name', 'enabled', 'users']);
  if (typeof v.enabled !== 'boolean' || !Array.isArray(v.users) || v.users.length > 100) invalid();
  const users = v.users
    .map((value) => {
      const u = fields(value, ['user_id', 'desktop_view', 'desktop_control']);
      if (
        !Number.isInteger(u.user_id) ||
        (u.user_id as number) < 1 ||
        (u.user_id as number) > 2147483647 ||
        typeof u.desktop_view !== 'boolean' ||
        typeof u.desktop_control !== 'boolean' ||
        (u.desktop_control && !u.desktop_view)
      )
        invalid();
      return { user_id: u.user_id as number, desktop_view: u.desktop_view, desktop_control: u.desktop_control };
    })
    .sort((a, b) => a.user_id - b.user_id);
  if (new Set(users.map((u) => u.user_id)).size !== users.length) invalid();
  return { status: deviceStatus(v.status), name: label(v.name, 80), enabled: v.enabled, users };
}
