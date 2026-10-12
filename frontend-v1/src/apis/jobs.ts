import { request } from '.';
export interface Job { id: string; kind: string; status: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'; progress: number; result: { file_id?: string; source_count?: number; translated_source_count?: number; project_id?: string }; last_error?: string; }
export const getJob = (id: string) => request<Job>({ method: 'GET', url: `/v1/jobs/${id}` });
export const cancelJob = (id: string) => request({ method: 'POST', url: `/v1/jobs/${id}/cancel` });
export async function waitForJob(id: string, options: { signal?: AbortSignal; onProgress?: (job: Job) => void } = {}): Promise<Job> {
  while (!options.signal?.aborted) {
    const { data: job } = await getJob(id);
    options.onProgress?.(job);
    if (job.status === 'succeeded') return job;
    if (job.status === 'failed' || job.status === 'cancelled') throw new Error(job.last_error || `Task ${job.status}`);
    await new Promise<void>((resolve) => { const timer = setTimeout(done, 1000); function done() { clearTimeout(timer); options.signal?.removeEventListener('abort', done); resolve(); } options.signal?.addEventListener('abort', done, { once: true }); });
  }
  throw new Error('Task observation cancelled');
}
