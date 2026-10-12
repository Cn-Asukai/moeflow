import type { SagaIterator } from 'redux-saga';
import { call, cancelled, put, select, takeLatest } from 'redux-saga/effects';
import { api, BasicSuccessResult, handleFailure } from '@/apis';
import { toLowerCamelCase } from '@/utils';
import { getCancelToken } from '@/utils/api';
import {
  clearCurrentProject,
  setCurrentProject,
  setCurrentProjectSaga,
} from './slice';
import { AppState } from '@/store';
import { Project } from '@/interfaces';

// worker Sage
function* setCurrentProjectWorker(
  action: ReturnType<typeof setCurrentProjectSaga>,
): SagaIterator {
  // 清空当前 project
  yield put(clearCurrentProject());
  const projects: Project[] = yield select(
    (state: AppState) => state.project.projects,
  );
  const project = projects.find(
    (project: Project) => project.id === action.payload.id,
  );
  if (project) {
    // 已存在与 projects 中，则直接获取
    yield put(setCurrentProject(project));
  } else {
    // 从 API 获取当前 project
    const [cancelToken, cancel] = getCancelToken();
    try {
      const result: BasicSuccessResult<Project> = yield call(api.project.getProject, {
        id: action.payload.id,
        configs: { cancelToken },
      });
      yield put(setCurrentProject(toLowerCamelCase(result.data)));
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
  yield takeLatest(setCurrentProjectSaga.type, setCurrentProjectWorker);
}

// root Saga
export default watcher;
