import type { SagaIterator } from 'redux-saga';
import { call, cancelled, put, select, takeLatest } from 'redux-saga/effects';
import { api, BasicSuccessResult, handleFailure } from '@/apis';
import { toLowerCamelCase } from '@/utils';
import { getCancelToken } from '@/utils/api';
import {
  clearCurrentProjectSet,
  setCurrentProjectSet,
  setCurrentProjectSetSaga,
} from './slice';
import { AppState } from '..';
import { UserProjectSet } from '@/interfaces';

// worker Sage
function* setCurrentProjectSetWorker(
  action: ReturnType<typeof setCurrentProjectSetSaga>,
): SagaIterator {
  // 清空当前 projectSet
  yield put(clearCurrentProjectSet());
  const projectSets: UserProjectSet[] = yield select(
    (state: AppState) => state.projectSet.projectSets,
  );
  const projectSet = projectSets.find(
    (projectSet: UserProjectSet) => projectSet.id === action.payload.id,
  );
  if (projectSet) {
    // 已存在与 projectSets 中，则直接获取
    yield put(setCurrentProjectSet(projectSet));
  } else {
    // 从 API 获取当前 projectSet
    const [cancelToken, cancel] = getCancelToken();
    try {
      const result: BasicSuccessResult<UserProjectSet> =
        yield call(api.projectSet.getProjectSet, {
          id: action.payload.id,
          configs: { cancelToken },
        });
      yield put(setCurrentProjectSet(toLowerCamelCase(result.data)));
    } catch (error) {
      handleFailure(error);
    } finally {
      if (yield cancelled()) {
        cancel();
      }
    }
  }
}

// watcher Saga
function* watcher(): SagaIterator {
  yield takeLatest(setCurrentProjectSetSaga.type, setCurrentProjectSetWorker);
}

// root Saga
export default watcher;
