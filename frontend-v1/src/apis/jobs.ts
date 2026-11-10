import { request } from '.';
import { getIntl } from '@/locales';
import type { IntlShape } from 'react-intl';
export interface Job { id: string; kind: string; status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'; progress: number; result: { file_id?: string; source_count?: number; translated_source_count?: number; project_id?: string }; last_error?: string; }
const JOB_STATUS_MESSAGE_ID: Record<string, string> = {
  queued: 'job.queued',
  preparing: 'job.preparing',
  running: 'job.running',
  succeeded: 'job.succeeded',
  failed: 'job.failed',
  skipped: 'job.skipped',
  cancelled: 'job.cancelled',
};
export function formatJobStatus(formatMessage: IntlShape['formatMessage'], status: string): string {
  const id = JOB_STATUS_MESSAGE_ID[status];
  return id ? String(formatMessage({ id })) : status;
}
export const getJob = (id: string) => request<Job>({ method: 'GET', url: `/v1/jobs/${id}` });
export const cancelJob = (id: string) => request({ method: 'POST', url: `/v1/jobs/${id}/cancel` });
export async function waitForJob(id: string, options: { signal?: AbortSignal; onProgress?: (job: Job) => void } = {}): Promise<Job> {
  while (!options.signal?.aborted) {
    const { data: job } = await getJob(id);
    options.onProgress?.(job);
    if (job.status === 'succeeded') return job;
    if (job.status === 'failed' || job.status === 'cancelled') {
      const status = getIntl().formatMessage({ id: JOB_STATUS_MESSAGE_ID[job.status] });
      throw new Error(job.last_error || getIntl().formatMessage({ id: 'job.failedStatus' }, { status }));
    }
    const { promise, resolve } = Promise.withResolvers<void>();
    const timer = setTimeout(done, 1000);
    function done() { clearTimeout(timer); options.signal?.removeEventListener('abort', done); resolve(); }
    options.signal?.addEventListener('abort', done, { once: true });
    await promise;
  }
  throw new Error(getIntl().formatMessage({ id: 'job.observationCancelled' }));
}
