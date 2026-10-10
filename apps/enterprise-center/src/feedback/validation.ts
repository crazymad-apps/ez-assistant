import { fields, invalid, page, queryBoolean, queryFields, recordId } from '../validation.js';
import { IdentityError } from '../identity/errors.js';
import { pluginName } from '../plugins/manifest.js';
import { feedbackKinds, feedbackSources, feedbackStatuses } from '../contracts/feedback.js';
import type { FeedbackContent, FeedbackEnvironment, SubmitFeedbackRequest, HandleFeedbackRequest } from '../contracts/feedback.js';

function text(value: unknown, max: number, required: true): string;
function text(value: unknown, max: number, required?: false): string | null;
function text(value: unknown, max: number, required = false): string | null {
  if (value == null && !required) return null;
  if (typeof value !== 'string' || /[\uD800-\uDFFF]/u.test(value) || value.includes('\0')) invalid();
  const result = value.trim();
  if ([...result].length > max || (required && !result)) invalid();
  return result || null;
}
function choice<T extends string>(value: unknown, choices: readonly T[]): T {
  if (typeof value !== 'string' || !choices.includes(value as T)) invalid();
  return value as T;
}
export function feedbackId(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)) invalid();
  return value.toLowerCase();
}
function bounded(value: object, max: number) {
  if (Buffer.byteLength(JSON.stringify(value)) > max) throw new IdentityError(413, 'FEEDBACK_TOO_LARGE');
}
export function submission(value: unknown): SubmitFeedbackRequest {
  const raw = fields(value, ['target', 'title', 'description', 'submission_id', 'source', 'environment'], ['reproduction', 'expected_result']);
  const target = fields(raw.target, ['kind', 'name'], ['reference', 'version', 'content_ref', 'location']);
  const kind = choice(target.kind, feedbackKinds);
  const reference = text(target.reference, 512);
  const name = text(target.name, 120, true);
  const version = text(target.version, 64);
  if (kind === 'plugin' && (!reference || !pluginName.test(reference) || reference.length > 64)) invalid();
  if (kind !== 'plugin' && version) invalid();
  if (kind === 'platform' && reference !== null && reference !== 'ez-assistant') invalid();
  const content: FeedbackContent = {
    target: { kind, reference: kind === 'platform' ? 'ez-assistant' : reference,
      name: kind === 'platform' ? 'EZ Assistant' : name, version,
      content_ref: text(target.content_ref, 512), location: text(target.location, 512) },
    title: text(raw.title, 120, true), description: text(raw.description, 4000, true),
    reproduction: text(raw.reproduction, 2000), expected_result: text(raw.expected_result, 2000),
  };
  bounded(content, 12 * 1024);
  const env = fields(raw.environment, ['client_kind'], ['host_version', 'host_os', 'host_arch', 'client_version', 'client_os', 'client_arch']);
  const environment: FeedbackEnvironment = {
    host_version: text(env.host_version, 128), host_os: text(env.host_os, 128), host_arch: text(env.host_arch, 128),
    client_kind: choice(env.client_kind, ['desktop', 'web', 'agent']), client_version: text(env.client_version, 128),
    client_os: text(env.client_os, 128), client_arch: text(env.client_arch, 128),
  };
  bounded(environment, 2048);
  return { ...content, submission_id: feedbackId(raw.submission_id), source: choice(raw.source, feedbackSources), environment };
}
export function handling(value: unknown): HandleFeedbackRequest {
  const raw = fields(value, ['expected_status', 'expected_handling_note', 'status', 'handling_note']);
  const status = choice(raw.status, feedbackStatuses);
  const note = text(raw.handling_note, 2000);
  if ((status === 'resolved' || status === 'closed') && !note) invalid();
  return { status, handling_note: note, expected_status: choice(raw.expected_status, feedbackStatuses),
    expected_handling_note: text(raw.expected_handling_note, 2000) };
}
export function feedbackQuery(value: unknown, admin: boolean) {
  const raw = queryFields(value, ['limit', 'offset', 'status', 'target_kind', 'target_reference', 'target_unresolved',
    ...(admin ? ['source', 'owner_user_id', 'title'] : ['submission_id'])]);
  const paging = page(raw);
  if (paging.limit > 20) invalid();
  const kind = raw.target_kind === undefined ? null : choice(raw.target_kind, feedbackKinds);
  const reference = raw.target_reference === undefined ? null : text(raw.target_reference, 512, true);
  const unresolved = raw.target_unresolved === undefined ? null : queryBoolean(raw.target_unresolved);
  if ((reference !== null && (!kind || unresolved !== null)) || (unresolved !== null && kind !== 'knowledge')) invalid();
  return { ...paging, kind, reference, unresolved,
    status: raw.status === undefined ? null : choice(raw.status, feedbackStatuses),
    source: raw.source === undefined ? null : choice(raw.source, feedbackSources),
    owner: raw.owner_user_id === undefined ? null : recordId(raw.owner_user_id),
    title: raw.title === undefined ? null : text(raw.title, 120, true),
    submission: raw.submission_id === undefined ? null : feedbackId(raw.submission_id),
  };
}
