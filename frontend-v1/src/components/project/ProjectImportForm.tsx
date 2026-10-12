import { css } from '@emotion/core';
import { Button, message, Upload } from 'antd';
import { useState } from 'react';
import { useIntl } from 'react-intl';
import { api } from '@/apis';
import { FC } from '@/interfaces';
import { useDispatch } from 'react-redux';
import { createProject, resetProjectsState } from '@/store/project/slice';
import { toLowerCamelCase } from '@/utils';
import { resetFilesState } from '@/store/file/slice';
import { RcFile } from 'antd/lib/upload';
import produce from 'immer';
import { waitForJob } from '@/apis/jobs';

/** 导入项目表单的属性接口 */
interface ProjectImportFormProps {
  teamID: string;
  projectSetID: string;
  className?: string;
}
/**
 * 导入项目表单
 */
export const ProjectImportForm: FC<ProjectImportFormProps> = ({
  teamID,
  projectSetID,
  className,
}) => {
  const { formatMessage } = useIntl(); // i18n
  const dispatch = useDispatch();
  const [importing, setImporting] = useState(false);
  const [importStatuses, setImportStatuses] = useState<string[]>([]);
  const [importFileList, setImportFileList] = useState<RcFile[]>();

  const handleImport = async () => {
    setImporting(true);
    setImportStatuses((importFileList || []).map((file) => `${file.name}: queued`));
    try {
      for (const [index, file] of (importFileList || []).entries()) {
        const update = (status: string) => setImportStatuses(produce((draft) => { draft[index] = `${file.name}: ${status}`; }));
        try {
          const result = await api.project.importProject({ teamID, projectSetID, data: { project: file }, configs: { onUploadProgress: (event) => update(`upload ${Math.round(event.loaded / file.size * 100)}%`) } });
          const job = await waitForJob(result.data.task_id, { onProgress: (job) => update(`${job.status} ${Math.round(job.progress)}%`) });
          if (job.result.project_id) {
            const project = await api.project.getProject({ id: job.result.project_id });
            dispatch(createProject({ project: toLowerCamelCase(project.data), unshift: true }));
          }
          update('succeeded');
          dispatch(resetFilesState());
          dispatch(resetProjectsState());
        } catch (error) {
          update(error instanceof Error ? error.message : 'Import failed');
          message.error(`${file.name}: Import failed`);
        }
      }
    } finally { setImporting(false); }
  };

  return (
    <div
      className={className}
      css={css`
        width: 100%;
        .ProjectImportForm__Zip {
          margin-bottom: 24px;
        }
        .ProjectImportForm__ZipUpload {
          display: flex;
          align-items: center;
        }
      `}
    >
      {!importing && importStatuses.map((status, index) => <div key={index}>{status}</div>)}
      {importing ? (
        <div>
          {importStatuses.map((status, i) => (
            <div key={i}>{status}</div>
          ))}
        </div>
      ) : (
        <>
          <div className="ProjectImportForm__Zip">
            <div className="ProjectImportForm__ZipUpload">
              <Upload
                accept=".zip"
                beforeUpload={() => {
                  return false;
                }}
                multiple
                disabled={importing}
                onChange={(file) => {
                  setImportFileList(
                    file.fileList.map((file) => file.originFileObj!),
                  );
                }}
              >
                <Button>{formatMessage({ id: 'site.selectFile' })}</Button>
              </Upload>
            </div>
          </div>
          <Button
            type="primary"
            block
            loading={importing}
            onClick={handleImport}
          >
            {formatMessage({ id: 'project.startImport' })}
          </Button>
        </>
      )}
    </div>
  );
};
