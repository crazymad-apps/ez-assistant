import * as sdk from './openapi/sdk.gen';
import type { HandleFeedbackRequest, ListAdminFeedbackData } from './openapi';

export type FeedbackQuery = NonNullable<ListAdminFeedbackData['query']>;
const options = (token: string, signal?: AbortSignal) => ({
  headers: { Authorization: `Bearer ${token}` },
  signal,
  throwOnError: true as const,
});
export const feedbackApi = {
  list: async (token: string, query: FeedbackQuery, signal: AbortSignal) =>
    (await sdk.listAdminFeedback({ ...options(token, signal), query })).data,
  get: async (token: string, id: string, signal: AbortSignal) =>
    (await sdk.getAdminFeedback({ ...options(token, signal), path: { id } })).data,
  handle: async (token: string, id: string, body: HandleFeedbackRequest) =>
    (await sdk.handleFeedback({ ...options(token), path: { id }, body })).data,
};
