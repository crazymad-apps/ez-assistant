import type { CreateUserRequest, UpdateUserRequest, UserQuery } from '../contracts/users.js';
import { password, username } from '../identity/validation.js';
import { fields, invalid, page, queryBoolean, queryFields } from '../validation.js';

function displayName(value: unknown): string {
  if (typeof value !== 'string' || !value.trim() || [...value].length > 64
    || [...value].some(character => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127)) invalid();
  return value.trim();
}
function role(value: unknown): 'user' | 'admin' { if (value !== 'user' && value !== 'admin') invalid(); return value; }
function enabled(value: unknown): boolean { if (typeof value !== 'boolean') invalid(); return value; }
export function createUserInput(value: unknown): CreateUserRequest {
  const input = fields(value, ['username', 'display_name', 'role', 'enabled', 'password']);
  return { username: username(input.username), display_name: displayName(input.display_name), role: role(input.role),
    enabled: enabled(input.enabled), password: password(input.password, true) };
}
export function updateUserInput(value: unknown): UpdateUserRequest {
  const input = fields(value, [], ['display_name', 'role', 'enabled']);
  if (!Object.keys(input).length) invalid();
  return { ...(Object.hasOwn(input, 'display_name') ? { display_name: displayName(input.display_name) } : {}),
    ...(Object.hasOwn(input, 'role') ? { role: role(input.role) } : {}),
    ...(Object.hasOwn(input, 'enabled') ? { enabled: enabled(input.enabled) } : {}) };
}
export function resetPasswordInput(value: unknown): string { return password(fields(value, ['new_password']).new_password, true); }

export function userQuery(value: unknown): UserQuery & { limit: number; offset: number } {
  const input = queryFields(value, ['search', 'role', 'enabled', 'limit', 'offset']);
  if (input.search !== undefined && [...input.search].length > 64) invalid();
  return { ...page(input), ...(input.search === undefined ? {} : { search: input.search.trim() }),
    ...(input.role === undefined ? {} : { role: role(input.role) }),
    ...(input.enabled === undefined ? {} : { enabled: queryBoolean(input.enabled) }) };
}
