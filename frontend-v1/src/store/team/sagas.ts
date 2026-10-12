import type { SagaIterator } from 'redux-saga';
import { call, cancelled, put, select, takeLatest } from 'redux-saga/effects';
import { api, BasicSuccessResult, handleFailure } from '@/apis';
import { toLowerCamelCase } from '../../utils';
import { getCancelToken } from '../../utils/api';
import { clearCurrentTeam, setCurrentTeam, setCurrentTeamSaga } from './slice';
import { AppState } from '..';
import { UserTeam } from '../../interfaces';

// worker Sage
function* setCurrentTeamWorker(action: ReturnType<typeof setCurrentTeamSaga>): SagaIterator {
  // 清空当前 team
  yield put(clearCurrentTeam());
  const teams: UserTeam[] = yield select((state: AppState) => state.team.teams);
  const team = teams.find((team: UserTeam) => team.id === action.payload.id);
  if (team) {
    // 已存在与 teams 中，则直接获取
    yield put(setCurrentTeam(team));
  } else {
    // 从 API 获取当前 team
    const [cancelToken, cancel] = getCancelToken();
    try {
      const result: BasicSuccessResult<UserTeam> = yield call(api.team.getTeam, {
        id: action.payload.id,
        configs: { cancelToken },
      });
      yield put(setCurrentTeam(toLowerCamelCase(result.data)));
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
  yield takeLatest(setCurrentTeamSaga.type, setCurrentTeamWorker);
}

// root Saga
export default watcher;
