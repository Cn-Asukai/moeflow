import { FC, useState } from 'react';
import { File as MFile, Target } from '@/interfaces';
import { useIntl } from 'react-intl';
import { ResourcePool } from '@jokester/ts-commonutil/lib/concurrency/resource-pool-basic';
import { useAsyncEffect } from '@jokester/ts-commonutil/lib/react/hook/use-async-effect';
import { api, request } from '@/apis';
import { cancelJob, waitForJob } from '@/apis/jobs';
import { toLowerCamelCase } from '@/utils';
import { ModalHandle } from '.';
import { Icon } from '../icon';

export const BatchTranslateModalContent: FC<{ files: MFile[]; target: Target; onFileSaved?(file: MFile): void; getHandle(): ModalHandle }> = ({ files, target, onFileSaved, getHandle }) => {
  const { formatMessage } = useIntl();
  const [states, setStates] = useState(files.map((file) => ({ file, status: 'queued', message: '' })));
  useAsyncEffect(async (running, released) => {
    const controller = new AbortController();
    const active = new Set<string>();
    released.then(() => { controller.abort(); active.forEach((id) => { void cancelJob(id).catch(() => undefined); }); });
    const update = (file: MFile, status: string, message: string) => {
      if (running.current) setStates((previous) => previous.map((state) => state.file.id === file.id ? { file, status, message } : state));
    };
    const limiter = ResourcePool.multiple([1, 2]);
    await Promise.allSettled(files.map((file) => limiter.use(async () => {
      if (!running.current) return;
      if (![undefined, null, 'success'].includes(file.uploadState)) { update(file, 'failed', formatMessage({ id: 'fileList.aiTranslate.fileMessage.uploadNotFinished' })); return; }
      try {
        const current = toLowerCamelCase((await api.file.getFile({ fileID: file.id })).data);
        if (current.sourceCount) { update(file, 'skipped', formatMessage({ id: 'fileList.aiTranslate.fileMessage.textAlreadyExist' })); return; }
        const { data } = await request<{ task_id: string }>({ method: 'POST', url: `/v1/files/${file.id}/ai`, data: { target_id: target.id } });
        active.add(data.task_id);
        if (!running.current) { await cancelJob(data.task_id); return; }
        const job = await waitForJob(data.task_id, { signal: controller.signal, onProgress: (job) => update(file, job.status, `${job.status} ${Math.round(job.progress)}%`) });
        active.delete(data.task_id);
        const saved = toLowerCamelCase((await api.file.getFile({ fileID: file.id })).data);
        update(file, 'succeeded', formatMessage({ id: 'fileList.aiTranslate.fileMessage.success' }, { count: job.result.source_count || 0 }));
        if (running.current) onFileSaved?.(saved);
      } catch (error) {
        update(file, 'failed', error instanceof Error ? error.message : formatMessage({ id: 'fileList.aiTranslate.fileMessage.translateFailed' }));
      }
    })));
    if (running.current) getHandle().update({ okButtonProps: { disabled: false } });
  }, []);
  return <div><p>AI uses the model configured by the server administrator. Closing this dialog requests cancellation of unfinished tasks.</p><p>{formatMessage({ id: 'fileList.aiTranslate.workingModal.content' }, { fileCount: files.length })}</p><ul>{states.map(({ file, status, message }) => <li key={file.id}><Icon icon={status === 'succeeded' ? 'check' : status === 'failed' || status === 'skipped' ? 'exclamation-circle' : status === 'running' ? 'spinner' : 'ellipsis-h'} spin={status === 'running'} /> {file.name} — {message || status}</li>)}</ul></div>;
};
