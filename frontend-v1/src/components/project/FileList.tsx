import { css, Global } from '@emotion/core';
import { Button as AntdButton, Drawer, message, Modal, Spin } from 'antd';
import loadImage from 'blueimp-load-image';
import classNames from 'classnames';
import { useEffect, useRef, useState } from 'react';
import { FilePond } from 'react-filepond';
import { useIntl } from 'react-intl';
import { useDispatch, useSelector } from 'react-redux';
import { useHistory } from 'react-router-dom';
import { Button, EmptyTip, List } from '@/components';
import { OutputList } from './OutputList';
import { FileItem } from './FileItem';
import { api, isFailureResult, resultTypes } from '@/apis';
import {
  FILE_NOT_EXIST_REASON,
  FILE_SAFE_STATUS,
  FILE_TYPE,
  IMAGE_COVER,
  PARSE_STATUS,
  PROJECT_PERMISSION,
  PROJECT_STATUS,
} from '@/constants';
import { FC, File as MFile, Project, Target } from '@/interfaces';
import { AppState } from '@/store';
import { setFilesState } from '@/store/file/slice';
import style from '@/style';
import { toLowerCamelCase } from '@/utils';
import { can } from '@/utils/user';
import { routes } from '@/pages/routes';
import { ListPageSpec } from '@/components/shared/List';
import { FilePondFile } from 'filepond';
import { createDebugLogger } from '@/utils/debug-logger';
import { useAiTranslate } from '@/components/ai';
import { directUpload } from '@/apis/upload';

/** 文件列表的属性接口 */
interface FileListProps {
  project: Project;
  onChangeTargetClick?: () => void;
  target: Target;
  className?: string;
}

const debugLogger = createDebugLogger('components:project:FileList');
/**
 * 文件列表
 */
export const FileList: FC<FileListProps> = ({
  project,
  target,
  onChangeTargetClick,
  className,
}) => {
  const { formatMessage } = useIntl(); // i18n
  const history = useHistory(); // 路由
  const dispatch = useDispatch();
  const [loading, setLoading] = useState(true);
  const [listMode] = useState<'image' | 'text'>('image');
  const [total, setTotal] = useState(0); // 元素总个数
  const platform = useSelector((state: AppState) => state.site.platform);
  const isMobile = platform === 'mobile';
  const [outputDrawerVisible, setOutputDrawerVisible] = useState(false);
  const coverWidth = IMAGE_COVER.WIDTH;
  const coverHeight = IMAGE_COVER.HEIGHT;

  const [items, setItems] = useState<MFile[]>([]);
  const [currentParentID, setCurrentParentID] = useState<string>();
  const [currentParentName, setCurrentParentName] = useState('');
  useEffect(() => { setCurrentParentID(undefined); setCurrentParentName(''); }, [project.id]);
  const visibleItems = items.filter((item) => currentParentID === undefined || item.parentID === currentParentID);
  const [spinningIDs, setSpinningIDs] = useState<string[]>([]); // 删除请求中
  const filePondRef = useRef<FilePond | null>();
  const currentPageSpecRef = useRef<ListPageSpec | null>(null);
  const pendingTextKey = items.filter((item) => item.type === FILE_TYPE.TEXT && !item.uploading && (item.parseStatus === PARSE_STATUS.QUEUING || item.parseStatus === PARSE_STATUS.PARSING)).map((item) => item.id).sort().join(',');
  useEffect(() => {
    if (!pendingTextKey || project.status === PROJECT_STATUS.FINISHED) return;
    const controller = new AbortController();
    const pending = new Set(pendingTextKey.split(','));
    let timer: number;
    async function poll() {
      await Promise.all(Array.from(pending, async (id) => {
        try {
          const response = await api.file.getFile({ fileID: id, params: { target: target.id }, configs: { signal: controller.signal } });
          if (controller.signal.aborted) return;
          const file = toLowerCamelCase(response.data);
          if ((file.parseStatus !== PARSE_STATUS.QUEUING && file.parseStatus !== PARSE_STATUS.PARSING) || file.activated === false) pending.delete(id);
          setItems((current) => file.activated === false ? current.filter((item) => item.id !== id) : current.map((item) => item.id === id && !item.uploading ? { ...item, ...file } : item));
        } catch (error: unknown) {
          if (!controller.signal.aborted && isFailureResult(error) && error.type === resultTypes.BASIC_FAILURE && error.data.code === 404) {
            pending.delete(id);
            setItems((current) => current.filter((item) => item.id !== id));
          }
        }
      }));
      if (!controller.signal.aborted && pending.size) timer = window.setTimeout(poll, 2000);
    }
    timer = window.setTimeout(poll, 1000);
    return () => { controller.abort(); clearTimeout(timer); };
  }, [pendingTextKey, project.id, project.status, target.id]);

  const defaultPage = useSelector(
    (state: AppState) => state.file.filesState.page,
  );
  const defaultWord = useSelector(
    (state: AppState) => state.file.filesState.word,
  );
  const defaultScrollTop = useSelector(
    (state: AppState) => state.file.filesState.scrollTop,
  );
  const selectedFileIds = useSelector(
    (state: AppState) => state.file.filesState.selectedFileIds,
  );
  const [aiEnabled, aiTranslateApi, aiModalHolder] = useAiTranslate(
    [...new Set(selectedFileIds)]
      .map((id) => items.find((item) => item.id === id))
      .filter(Boolean) as MFile[],
    target,
  );

  const openInTranslator = (file: MFile) => {
    history.push(routes.imageTranslator.build(file.id, target.id));
  };

  const deleteFile = (file: MFile) => {
    Modal.confirm({
      title: formatMessage({ id: 'project.deleteFile' }),
      content: formatMessage(
        { id: 'project.deleteFileTip' },
        { name: file.name },
      ),
      onOk: () => {
        setSpinningIDs((ids) => [file.id, ...ids]);
        api.file
          .deleteFile({
            id: file.id,
          })
          .then((result) => {
            message.success(result.data.message);
            // 修改数据
            setItems((items) => {
              return items.filter((item) => item.id !== file.id);
            });
          })
          .catch((error) => {
            error.default();
          })
          .finally(() => {
            setSpinningIDs((ids) => ids.filter((id) => id !== file.id));
          });
      },
      onCancel: () => {},
      okText: formatMessage({ id: 'form.ok' }),
      cancelText: formatMessage({ id: 'form.cancel' }),
    });
  };

  /** 处理文件添加 */
  const handleFileAdd = (error: any, file: FilePondFile) => {
    file.setMetadata('parentID', currentParentID || null);
    // 加载预览图
    const realFile = file.file;
    if (/^image.+/.test(file.file.type)) {
      loadImage(
        realFile,
        (img) => {
          setItems((items) =>
            items.map((item) => {
              if (item.id === file.id && img) {
                return {
                  ...item,
                  coverUrl: (img as HTMLCanvasElement).toDataURL(),
                };
              }
              return item;
            }),
          );
        },
        {
          maxWidth: coverWidth,
          maxHeight: coverHeight,
          crop: true,
          canvas: true,
          ...({ imageSmoothingQuality: 'high' } as any), // @type 版本太旧
        },
      );
    }
    const uploadingFile: MFile = {
      // this is the random id generated by filepond
      id: file.id,
      name: file.filename,
      saveName: '',
      type: FILE_TYPE.IMAGE,
      sourceCount: 0,
      translatedSourceCount: 0,
      checkedSourceCount: 0,
      fileNotExistReason: FILE_NOT_EXIST_REASON.UNKNOWN,
      safeStatus: FILE_SAFE_STATUS.NEED_MACHINE_CHECK,
      parseStatus: PARSE_STATUS.NOT_START,
      parseStatusDetailName: formatMessage({ id: 'file.parseNotStart' }),
      parseErrorTypeDetailName: '',
      url: '',
      parentID: currentParentID || null,
      fileTargetCache: {
        translatedSourceCount: 0,
        checkedSourceCount: 0,
      },
      uploading: true,
      uploadState: 'uploading',
      uploadPercent: 0,
    };
    setItems((items) => [uploadingFile, ...items]);
  };

  /** 获取元素 */
  const loadPage = ({ page, pageSize, word, cancelToken }: ListPageSpec) => {
    setLoading(true);
    currentPageSpecRef.current = {
      page,
      pageSize,
      word: word || '',
      cancelToken,
    };
    return api.file
      .getProjectFiles({
        projectID: project.id,
        params: {
          page,
          limit: pageSize,
          word,
          target: target.id,
          parentID: currentParentID,
        },
        configs: {
          cancelToken,
        },
      })
      .then((result) => {
        const data = (result.data as MFile[]).map((d) => toLowerCamelCase(d));
        setItems((current) => [...current.filter((item) => item.uploading && !data.some((file) => file.id === item.id)), ...data]);
        setTotal(Number(result.headers['x-pagination-count']));
        setLoading(false);
      })
      .catch((error) => {
        // 如果是 cancel 的请求，则不取消 loading 状态，因为肯定有下一个请求
        if (error.type !== resultTypes.CANCEL_FAILURE) {
          setLoading(false);
        }
        error.default();
      });
  };
  /*

  const startOCR = () => {
    Modal.confirm({
      title: formatMessage({ id: 'project.startOCR' }),
      content: formatMessage({ id: 'project.startOCRTip' }),
      onOk: () => {
        apis
          .startProjectOCR({ id: project.id })
          .then((result) => {
            // TODO 自动刷新实现后删除此备注
            message.success(
              result.data.message + '（请稍后刷新页面查看进度）',
              2
            );
          })
          .catch((error) => {
            error?.default();
          });
      },
      onCancel: () => {},
      okText: formatMessage({ id: 'form.ok' }),
      cancelText: formatMessage({ id: 'form.cancel' }),
    });
  };
  // */

  debugLogger('items', items);
  return (
    <div
      className={classNames('FileList', className, {
        'FileList--image': listMode === 'image',
      })}
      css={css`
        flex: auto;
        width: 100%;
        height: 100%;
        display: flex;
        flex-direction: column;
        justify-content: flex-start;
        align-items: stretch;
        .FileList__Header {
          width: 100%;
          height: 45px;
          flex: none;
        }
        .FileList__SubHeader {
        }
        .FileList__List {
          .List__Items {
            padding: 0 ${style.paddingBase}px;
            .List__ItemWrapper {
              flex: none;
              margin-bottom: 10px;
            }
          }
        }
        .FileList__FileItem {
          margin: 0 auto;
        }
        .FileList__Header {
          display: flex;
          justify-content: stretch;
        }
        .FileList__ChangeTargetButton {
          flex: auto;
          .Button__Content {
            justify-content: flex-start;
          }
        }
      `}
    >
      <Global
        styles={css`
          .ant-drawer-body {
            padding: 0 !important;
          }
        `}
      />
      <FilePond
        name="file"
        disabled={!can(project, PROJECT_PERMISSION.ADD_FILE)}
        className="FileList__FilePond"
        ref={(ref) => (filePondRef.current = ref)}
        css={css`
          display: none;
        `}
        dropOnPage
        dropOnElement={false}
        allowMultiple
        maxParallelUploads={5}
        onaddfile={handleFileAdd}
        onprocessfilestart={(file) => setItems((current) => current.map((item) => item.id === file.id ? { ...item, uploading: true, uploadState: 'uploading', uploadPercent: 0 } : item))}
        // 上传中
        onprocessfileprogress={(file, progress) => {
          setItems((items) =>
            items.map((item) => {
              if (item.id === file.id) {
                return {
                  ...item,
                  uploadPercent: Math.floor(progress * 100),
                };
              }
              return item;
            }),
          );
        }}
        // 上传成功
        onprocessfile={(error, file) => {
          if (error) return;
          const result = toLowerCamelCase(JSON.parse(file.serverId) as MFile);
          setItems((items) => {
            const revision = result.revision;
            if (result.revisionFamilyID && revision !== undefined && items.some((item) => item.id !== file.id && item.revisionFamilyID === result.revisionFamilyID && item.revision !== undefined && item.revision > revision)) {
              return items.filter((item) => item.id !== file.id);
            }
            // 覆盖时删除列表中原来的文件
            const itemsWithoutSameID = items.filter(
              (item) => item.id === file.id || (item.id !== result.id && (!result.revisionFamilyID || item.revisionFamilyID !== result.revisionFamilyID)),
            );
            const updated = itemsWithoutSameID.map((item) => {
              if (item.id === file.id) {
                // return a new MFile item , overwriting its (Filepond-created) id
                return {
                  ...item,
                  ...result,
                  uploadState: 'success',
                  uploading: false,
                } as MFile;
              }
              return item;
            });
            debugLogger('updated on upload success', updated);
            return updated;
          });
        }}
        // 上传失败
        onerror={(error, file) => {
          if (file && file.id) {
            setItems((items) =>
              items.map((item) => {
                if (item.id === file.id) {
                  return {
                    ...item,
                    uploadState: 'failure',
                  };
                }
                return item;
              }),
            );
          }
        }}
        server={{
          process: (_name, file, _metadata, load, error, progress, abort) => {
            const controller = new AbortController();
            directUpload(file, { purpose: 'file', project_id: project.id, parent_id: typeof _metadata.parentID === 'string' ? _metadata.parentID : undefined, name: file.name }, {
              signal: controller.signal,
              onUploadProgress: (event) => progress(!!event.total, event.loaded, event.total || file.size),
            }).then((result) => load(JSON.stringify(result.data))).catch((failure) => {
              if (!controller.signal.aborted) error(failure?.data?.message || failure?.message || 'Upload failed');
            });
            return { abort: () => { controller.abort(); abort(); } };
          },
        }}
      />
      <div className="FileList__Header">
        {currentParentID && <Button onClick={() => { setCurrentParentID(undefined); setCurrentParentName(''); dispatch(setFilesState({ page: 1, scrollTop: 0 })); }}>Back to all files · {currentParentName}</Button>}
        <Button
          className="FileList__ChangeTargetButton"
          icon="exchange-alt"
          iconProps={{
            style: { height: '16px', width: '16px' },
          }}
          onClick={onChangeTargetClick}
        >
          {(!isMobile
            ? formatMessage({ id: 'project.changeTarget' }) + ' - '
            : '') + target?.language.i18nName}
        </Button>
        {aiEnabled && aiTranslateApi && can(project, PROJECT_PERMISSION.ADD_LABEL) && can(project, PROJECT_PERMISSION.ADD_TRA) && (
          <Button
            tooltipProps={{
              overlay: formatMessage({ id: 'fileList.aiTranslate.buttonTip' }),
            }}
            icon="robot"
            onClick={() =>
              aiTranslateApi.start({
                onFileSaved(f2) {
                  setItems((items) =>
                    items.map((f) => (f.id === f2.id ? f2 : f)),
                  );
                },
              })
            }
          >
            {formatMessage({ id: 'fileList.aiTranslate.buttonText' })}
          </Button>
        )}
        {/* {can(team, TEAM_PERMISSION.USE_OCR_QUOTA) && (
        )} */}
        {/* <Button
          tooltipProps={{
            overlay: formatMessage({ id: 'fileList.changeMode' }),
          }}
          icon={listMode === 'text' ? 'th-large' : 'th-list'}
          onClick={() => {
            setListMode(listMode === 'text' ? 'image' : 'text');
          }}
        ></Button> */}
        {can(project, PROJECT_PERMISSION.OUTPUT_TRA) && (
          <Button icon="download" onClick={() => setOutputDrawerVisible(true)}>
            {!isMobile && formatMessage({ id: 'project.export' })}
            {selectedFileIds.length > 0 && ` (${selectedFileIds.length})`}
          </Button>
        )}
        {can(project, PROJECT_PERMISSION.ADD_FILE) && (
          <Button
            icon="plus"
            onClick={() => {
              filePondRef.current?.browse();
            }}
          >
            {!isMobile && formatMessage({ id: 'site.upload' })}
          </Button>
        )}
      </div>
      {selectedFileIds.length > 0 && (
        <div className="FileList__SubHeader">
          <AntdButton
            type="link"
            onClick={() => {
              const idsOnThisPage = visibleItems.map((item) => item.id);
              const selectedIdsOnOtherPages = selectedFileIds.filter(
                (id) => !idsOnThisPage.includes(id),
              );
              dispatch(
                setFilesState({
                  selectedFileIds: [
                    ...selectedIdsOnOtherPages,
                    ...idsOnThisPage,
                  ],
                }),
              );
            }}
          >
            {formatMessage({ id: 'site.selectPageAll' })}
          </AntdButton>
          <AntdButton
            type="link"
            onClick={() => {
              const idsOnThisPage = visibleItems.map((item) => item.id);
              const selectedIdsOnThisPage = selectedFileIds.filter((id) =>
                idsOnThisPage.includes(id),
              );
              const selectedIdsOnOtherPages = selectedFileIds.filter(
                (id) => !idsOnThisPage.includes(id),
              );
              const invertIds = idsOnThisPage.filter(
                (id) => !selectedIdsOnThisPage.includes(id),
              );
              dispatch(
                setFilesState({
                  selectedFileIds: [...selectedIdsOnOtherPages, ...invertIds],
                }),
              );
            }}
          >
            {formatMessage({ id: 'site.selectPageInverse' })}
          </AntdButton>
          <AntdButton
            type="link"
            onClick={() => {
              dispatch(setFilesState({ selectedFileIds: [] }));
            }}
          >
            {formatMessage({ id: 'site.selectCancel' })}
          </AntdButton>
        </div>
      )}
      <List
        key={`${project.id}:${currentParentID || 'all'}`}
        id={`${project.id}:${currentParentID || 'all'}`}
        className="FileList__List"
        onChange={loadPage}
        loading={loading}
        total={total}
        items={visibleItems}
        itemHeight={250}
        multiColumn
        columnWidth={200}
        paginationProps={{
          disabled: spinningIDs.length > 0,
        }}
        minPageSize={16}
        itemCreater={(file) => {
          return (
            <Spin spinning={spinningIDs.indexOf(file.id) > -1}>
              <FileItem
                className="FileList__FileItem"
                file={file}
                hasTarget={Boolean(target)}
                onClick={() => {
                  if (file.uploadState === 'failure') {
                    const retry = filePondRef.current?.processFile(file.id);
                    if (retry) void retry.catch(() => message.error('Upload retry failed. Click the file to try again.'));
                    return;
                  }
                  if (file.type === FILE_TYPE.FOLDER) {
                    setCurrentParentID(file.id);
                    setCurrentParentName(file.name);
                    dispatch(setFilesState({ page: 1, scrollTop: 0 }));
                    return;
                  }
                  (file.uploadState === undefined ||
                    file.uploadState === 'success') &&
                    openInTranslator(file);
                }}
                selectVisible={can(project, PROJECT_PERMISSION.OUTPUT_TRA)}
                selected={selectedFileIds.includes(file.id)}
                onSelect={(value) => {
                  if (value) {
                    dispatch(
                      setFilesState({
                        selectedFileIds: [...selectedFileIds, file.id],
                      }),
                    );
                  } else {
                    dispatch(
                      setFilesState({
                        selectedFileIds: selectedFileIds.filter(
                          (id) => id !== file.id,
                        ),
                      }),
                    );
                  }
                }}
                deleteButtonVisible={
                  file.uploadState === 'failure' || (can(project, PROJECT_PERMISSION.DELETE_FILE) &&
                  (file.uploadState === undefined || file.uploadState === 'success'))
                }
                onDeleteButtonClick={() => {
                  if (file.uploadState === 'failure') {
                    const removal = filePondRef.current?.removeFile(file.id, { revert: false });
                    void Promise.resolve(removal).catch(() => message.error('Could not remove local upload'));
                    setItems((current) => current.filter((item) => item.id !== file.id));
                    dispatch(setFilesState({ selectedFileIds: selectedFileIds.filter((id) => id !== file.id) }));
                    return;
                  }
                  deleteFile(file);
                }}
              />
            </Spin>
          );
        }}
        emptyTipCreater={() => {
          return (
            <EmptyTip
              className="ProjectList__EmptyTip"
              text={
                <>
                  <p>{formatMessage({ id: 'file.uploadTip1' })}</p>
                  <p>{formatMessage({ id: 'file.uploadTip2' })}</p>
                </>
              }
            />
          );
        }}
        searchEmptyTipCreater={(word) => {
          return (
            <EmptyTip
              className="ProjectList__EmptyTip"
              text={formatMessage({ id: 'file.emptySearchTip' }, { word })}
            />
          );
        }}
        defaultPage={currentParentID ? 1 : defaultPage}
        onPageChange={(page) => {
          dispatch(setFilesState({ page }));
        }}
        defaultWord={defaultWord}
        onWordChange={(word) => {
          dispatch(setFilesState({ word }));
        }}
        defaultScrollTop={defaultScrollTop}
        onScrollTopChange={(scrollTop) => {
          dispatch(setFilesState({ scrollTop }));
        }}
      />
      <Drawer
        className="FileList__OutputDrawer"
        title={
          formatMessage({ id: 'project.export' }) +
          ` - ${target.language.i18nName}`
        }
        placement={isMobile ? 'bottom' : 'right'}
        onClose={() => setOutputDrawerVisible(false)}
        open={outputDrawerVisible}
        width={350}
        height="70%"
        destroyOnClose={true}
      >
        <OutputList
          projectID={project.id}
          targetID={target.id}
          selectedFileIds={selectedFileIds}
        />
      </Drawer>
      {aiModalHolder}
    </div>
  );
};
