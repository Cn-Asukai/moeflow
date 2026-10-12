import type { SagaIterator } from 'redux-saga';
import { ExclamationCircleOutlined } from '@ant-design/icons';
import { Modal } from 'antd';
import { createElement } from 'react';
import {
  call,
  cancelled,
  delay,
  put,
  select,
  takeEvery,
  takeLatest,
  takeLeading,
} from 'redux-saga/effects';
import { v4 as uuidv4 } from 'uuid';
import { AppState } from '..';
import { api, BasicSuccessResult, handleFailure, isFailureResult, resultTypes } from '@/apis';
import { APISource } from '@/apis/source';
import { APITranslation } from '@/apis/translation';
import { SOURCE_POSITION_TYPE } from '@/constants/source';
import { Source } from '@/interfaces';
import { getIntl } from '@/locales';
import { toLowerCamelCase } from '@/utils';
import { getCancelToken } from '@/utils/api';
import { takeLatestPerKey } from '../helpers';
import { focusTranslation } from '../translation/slice';
import { UserState } from '../user/slice';
import {
  createSource,
  createSourceSaga,
  deleteSource,
  deleteSourceSaga,
  editMyTranslationSaga,
  editProofread,
  editProofreadSaga,
  editSource,
  editSourceSaga,
  fetchSourcesSaga,
  focusSource,
  rerankSource,
  rerankSourceSaga,
  selectTranslation,
  selectTranslationSaga,
  editProofreadContentStatuses,
  setSources,
  setSourcesLoading,
  editSourceMyTranslationContent,
  batchSelectTranslationSaga,
  setBatchSelecting,
  deleteTranslation,
} from './slice';
const inputDebounceDelay = 500;

// worker Sage
function* fetchSourcesWorker(action: ReturnType<typeof fetchSourcesSaga>): SagaIterator {
  yield put(setSourcesLoading(true));
  yield put(setSources([]));
  const [cancelToken, cancel] = getCancelToken();
  try {
    const result: BasicSuccessResult<APISource[]> = yield call(api.source.getSources,
      {
        fileID: action.payload.fileID,
        params: { targetID: action.payload.targetID },
        configs: { cancelToken },
      },
    );
    const data: Source[] = result.data.map((source) => ({
      ...toLowerCamelCase(source),
      isTemp: false,
      focus: false,
      selecting: false,
      labelStatus: 'pending' as 'pending',
      proodreadContentStatuses: {},
    }));
    yield put(setSources(data));
  } catch (error) {
    handleFailure(error);
  } finally {
    if (yield cancelled()) {
      cancel();
    }
    yield put(setSourcesLoading(false));
  }
}

function* createSourceWorker(action: ReturnType<typeof createSourceSaga>): SagaIterator {
  const tempID = uuidv4();
  yield put(
    createSource({
      id: tempID,
      x: action.payload.x,
      y: action.payload.y,
      isTemp: true,
      labelStatus: 'creating',
      proodreadContentStatuses: {},
      focus: false,
      selecting: false,
      content: '',
      translations: [],
      positionType: action.payload.positionType,
      hasOtherLanguageTranslation: false,
      tips: [],
    }),
  );
  yield put(
    focusSource({
      id: tempID,
      effects: [],
      noises: [],
    }),
  );
  const [cancelToken, cancel] = getCancelToken();
  const { fileID, ...requestData } = action.payload;
  try {
    const result: BasicSuccessResult<APISource> = yield call(api.source.createSource,
      {
        fileID,
        data: requestData,
        configs: { cancelToken },
      },
    );
    const data = toLowerCamelCase(result.data);
    yield put(
      editSource({
        ...data,
        id: tempID,
        newID: data.id,
        labelStatus: 'pending',
        isTemp: false,
        focus: false,
      }),
    );
    const focusedSourceID: string = yield select(
      (state: AppState) => state.source.focusedSource.id,
    );
    if (focusedSourceID === tempID) {
      yield put(
        focusSource({
          id: data.id,
          effects: ['focusInput', 'scrollIntoView'],
          noises: ['focusInput'],
        }),
      );
    }
  } catch (error) {
    handleFailure(error);
    yield put(deleteSource({ id: tempID }));
  } finally {
    if (yield cancelled()) {
      cancel();
    }
  }
}

function* editSourceWorker(action: ReturnType<typeof editSourceSaga>): SagaIterator {
  yield put(
    editSource({
      id: action.payload.id,
      labelStatus: 'saving',
    }),
  );
  if (action.payload.positionType !== undefined) {
    yield put(
      editSource({
        id: action.payload.id,
        positionType: action.payload.positionType,
      }),
    );
  }
  const [cancelToken, cancel] = getCancelToken();
  try {
    const result: BasicSuccessResult<APISource> = yield call(api.source.editSource, {
      sourceID: action.payload.id,
      data: action.payload,
      configs: { cancelToken },
    });
    const data: APISource = toLowerCamelCase(result.data);
    yield put(
      editSource({
        ...data,
        labelStatus: 'pending',
      }),
    );
  } catch (error) {
    if (
      isFailureResult(error) &&
      error.type === resultTypes.BASIC_FAILURE &&
      error.data.code === 8005 // label 已被删除
    ) {
      yield put(deleteSource({ id: action.payload.id }));
    } else {
      yield put(
        editSource({
          id: action.payload.id,
          labelStatus: 'pending',
        }),
      );
      // 还原标签位置
      action.payload.reset?.();
      // 还原标签位置分组
      if (action.payload.positionType !== undefined) {
        yield put(
          editSource({
            id: action.payload.id,
            positionType:
              action.payload.positionType === SOURCE_POSITION_TYPE.IN
                ? SOURCE_POSITION_TYPE.OUT
                : SOURCE_POSITION_TYPE.IN,
          }),
        );
      }
    }
    handleFailure(error);
  } finally {
    if (yield cancelled()) {
      cancel();
    }
  }
}

function* deleteSourceWorker(action: ReturnType<typeof deleteSourceSaga>): SagaIterator {
  const source: Source = yield select((state: AppState) =>
    state.source.sources.find((source) => source.id === action.payload.id),
  );
  if (
    source.myTranslation ||
    source.translations.length > 0 ||
    source.hasOtherLanguageTranslation
  ) {
    const stop: boolean = yield call(() => new Promise<boolean>((resolve) => {
      const intl = getIntl();
      Modal.confirm({
        icon: createElement(ExclamationCircleOutlined),
        title: intl.formatMessage({ id: 'imageTranslator.deleteLabelTitle' }),
        content: intl.formatMessage({ id: 'imageTranslator.deleteLabelTip' }),
        onOk() {
          resolve(false);
        },
        onCancel() {
          resolve(true);
        },
      });
    }));
    if (stop) {
      return;
    }
  }
  yield put(
    editSource({
      id: action.payload.id,
      labelStatus: 'deleting',
    }),
  );
  const [cancelToken, cancel] = getCancelToken();
  try {
    yield call(api.source.deleteSource, {
      sourceID: action.payload.id,
      configs: { cancelToken },
    });
    yield put(deleteSource({ id: action.payload.id }));
    yield put(focusSource({ id: '', effects: [], noises: [] }));
    yield put(focusTranslation({ id: '' }));
  } catch (error) {
    if (
      isFailureResult(error) &&
      error.type === resultTypes.BASIC_FAILURE &&
      error.data.code === 8005 // label 已被删除
    ) {
      yield put(deleteSource({ id: action.payload.id }));
      yield put(focusSource({ id: '', effects: [], noises: [] }));
      yield put(focusTranslation({ id: '' }));
    }
    handleFailure(error);
  } finally {
    if (yield cancelled()) {
      cancel();
    }
    yield put(
      editSource({
        id: action.payload.id,
        labelStatus: 'pending',
      }),
    );
  }
}

function* rerankSourceWorker(action: ReturnType<typeof rerankSourceSaga>): SagaIterator {
  const { id, next_source_id } = action.payload;

  yield call(api.source.rerankSource, {
    sourceID: id,
    nextSourceID: next_source_id,
  });
  yield put(rerankSource({ id, next_source_id }));
  yield put(focusSource({ id }));
}

// 翻译部分

function* editMyTranslationWorker(
  action: ReturnType<typeof editMyTranslationSaga>,
): SagaIterator {
  const { sourceID, content, targetID, focus = false } = action.payload;

  yield put(
    editSourceMyTranslationContent({
      sourceID,
      content,
      editTime: new Date().toISOString(),
    }),
  );

  if (!action.payload.noDebounce) {
    yield put(
      editSource({
        id: sourceID,
        myTranslationContentStatus: 'debouncing',
      }),
    );
    yield delay(inputDebounceDelay);
  }

  const [cancelToken, cancel] = getCancelToken();
  try {
    yield put(
      editSource({
        id: sourceID,
        myTranslationContentStatus: 'saving',
      }),
    );
    const result: BasicSuccessResult<APITranslation> =
      yield call(api.translation.createTranslation, {
        sourceID: sourceID,
        data: {
          content,
          targetID,
        },
        configs: { cancelToken },
      });
    const data: APITranslation = toLowerCamelCase(result.data);
    yield put(
      editSource({
        id: sourceID,
        myTranslation: data ? data : undefined,
        myTranslationContentStatus: 'saveSuccessful',
      }),
    );
    if (focus) {
      const focusedSourceID: string = yield select(
        (state: AppState) => state.source.focusedSource.id,
      );
      if (focusedSourceID === sourceID) {
        yield put(focusTranslation({ id: data ? data.id : '' }));
      }
    }
  } catch (error) {
    handleFailure(error);
    yield put(
      editSource({ id: sourceID, myTranslationContentStatus: 'saveFailed' }),
    );
  } finally {
    if (yield cancelled()) {
      cancel();
    }
  }
}

function* batchSelectTranslationWorker(
  action: ReturnType<typeof batchSelectTranslationSaga>,
): SagaIterator {
  const { fileID, data } = action.payload;
  const [cancelToken, cancel] = getCancelToken();
  try {
    yield put(setBatchSelecting(true));
    yield call(api.translation.batchSelectTranslation, {
      fileID,
      data,
      configs: {
        cancelToken,
      },
    });
    // 将所有 sources 修改成 selected
    const currentUser: UserState = yield select(
      (state: AppState) => state.user,
    );
    for (const { sourceID, translationID } of data) {
      yield put(
        selectTranslation({
          sourceID,
          translationID,
          selected: true,
          selector: currentUser,
        }),
      );
    }
  } catch (error) {
    handleFailure(error);
  } finally {
    if (yield cancelled()) {
      cancel();
    }
    yield put(setBatchSelecting(false));
  }
}

function* selectTranslationWorker(
  action: ReturnType<typeof selectTranslationSaga>,
): SagaIterator {
  const { sourceID, translationID, selected } = action.payload;
  const [cancelToken, cancel] = getCancelToken();
  try {
    yield put(editSource({ id: sourceID, selecting: true }));
    const result: BasicSuccessResult<APITranslation> =
      yield call(api.translation.editTranslation, {
        translationID,
        data: { selected },
        configs: {
          cancelToken,
        },
      });
    const data: APITranslation = toLowerCamelCase(result.data);
    yield put(
      selectTranslation({
        sourceID,
        translationID,
        selected,
        selector: data.selector,
      }),
    );
  } catch (error) {
    handleFailure(error);
  } finally {
    if (yield cancelled()) {
      cancel();
    }
    yield put(editSource({ id: sourceID, selecting: false }));
  }
}

function* editProofreadWorker(action: ReturnType<typeof editProofreadSaga>): SagaIterator {
  const { sourceID, translationID, proofreadContent } = action.payload;
  const currentUser: UserState = yield select((state: AppState) => state.user);

  yield put(
    editProofread({
      sourceID,
      translationID,
      proofreadContent,
      proofreader: currentUser,
      editTime: new Date().toISOString(),
    }),
  );

  if (!action.payload.noDebounce) {
    yield put(
      editProofreadContentStatuses({
        sourceID,
        translationID,
        status: 'debouncing',
      }),
    );
    yield delay(inputDebounceDelay);
  }

  const [cancelToken, cancel] = getCancelToken();
  try {
    yield put(
      editProofreadContentStatuses({
        sourceID,
        translationID,
        status: 'saving',
      }),
    );
    const result: BasicSuccessResult<APITranslation> =
      yield call(api.translation.editTranslation, {
        translationID,
        data: {
          proofreadContent,
        },
        configs: {
          cancelToken,
        },
      });
    const data: APITranslation = toLowerCamelCase(result.data);
    yield put(
      editProofreadContentStatuses({
        sourceID,
        translationID,
        status: 'saveSuccessful',
      }),
    );
    if (data) {
      yield put(
        editProofread({
          sourceID,
          translationID,
          proofreadContent: data.proofreadContent,
          proofreader: data.proofreader,
          editTime: data.editTime,
        }),
      );
    } else {
      yield put(focusTranslation({ id: '' }));
      yield put(
        deleteTranslation({
          sourceID,
          translationID,
        }),
      );
    }
  } catch (error) {
    handleFailure(error);
    yield put(
      editProofreadContentStatuses({
        sourceID,
        translationID,
        status: 'saveFailed',
      }),
    );
  } finally {
    if (yield cancelled()) {
      cancel();
    }
  }
}

// watcher Saga
function* watcher(): SagaIterator {
  yield takeLatest(fetchSourcesSaga.type, fetchSourcesWorker);
  yield takeEvery(createSourceSaga.type, createSourceWorker);
  yield takeLatestPerKey(
    editSourceSaga.type,
    editSourceWorker,
    (action: ReturnType<typeof editSourceSaga>) => {
      return action.payload.id;
    },
  );
  yield takeEvery(rerankSourceSaga.type, rerankSourceWorker);
  yield takeEvery(deleteSourceSaga.type, deleteSourceWorker);
  yield takeLeading(
    batchSelectTranslationSaga.type,
    batchSelectTranslationWorker,
  );
  // 翻译部分
  yield takeLatestPerKey(
    editMyTranslationSaga.type,
    editMyTranslationWorker,
    (action: ReturnType<typeof editMyTranslationSaga>) => {
      return action.payload.sourceID + action.payload.targetID;
    },
  );
  yield takeLatestPerKey(
    selectTranslationSaga.type,
    selectTranslationWorker,
    (action: ReturnType<typeof selectTranslationSaga>) => {
      return action.payload.sourceID;
    },
  );
  yield takeLatestPerKey(
    editProofreadSaga.type,
    editProofreadWorker,
    (action: ReturnType<typeof editProofreadSaga>) => {
      return action.payload.translationID;
    },
  );
}

// root Saga
export default watcher;
