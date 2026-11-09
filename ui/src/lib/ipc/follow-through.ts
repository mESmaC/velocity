import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  PR_OPENED_EVENT,
  PR_CI_EVENT,
  PR_REVIEW_EVENT,
  PR_MERGED_EVENT,
  PR_CLOSED_EVENT,
  type PullRequestLifecycleEvent,
} from '../bindings';

export type {
  PullRequestHistoryView,
  PullRequestLifecycleEvent,
} from '../bindings';

export const onPullRequestOpened = (
  callback: EventCallback<PullRequestLifecycleEvent>,
) => listen(PR_OPENED_EVENT, callback);
export const onPullRequestCi = (
  callback: EventCallback<PullRequestLifecycleEvent>,
) => listen(PR_CI_EVENT, callback);
export const onPullRequestReview = (
  callback: EventCallback<PullRequestLifecycleEvent>,
) => listen(PR_REVIEW_EVENT, callback);
export const onPullRequestMerged = (
  callback: EventCallback<PullRequestLifecycleEvent>,
) => listen(PR_MERGED_EVENT, callback);
export const onPullRequestClosed = (
  callback: EventCallback<PullRequestLifecycleEvent>,
) => listen(PR_CLOSED_EVENT, callback);
