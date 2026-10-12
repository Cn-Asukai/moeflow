import type { SagaIterator } from 'redux-saga';
import { call, put, takeEvery } from 'redux-saga/effects';
import { initialState, setUserInfo, setUserToken } from './slice';
import { toLowerCamelCase } from '../../utils';
import { api, BasicSuccessResult, handleFailure } from '@/apis';
import { setToken, removeToken } from '../../utils/cookie';
import { GetUserInfoResponse } from '../../apis/auth';
import type { Axios } from 'axios';

// worker Sage
function* getUserInfoAsync(action: ReturnType<typeof setUserToken>): SagaIterator {
  const token = action.payload.token;
  const instance: Axios = yield call(api.getAxiosInstance);
  if (token === '') {
    if (instance.defaults.headers.common['Authorization']) {
      yield call(() => instance.delete('/v1/user/token').catch(() => undefined));
    }
    // 清除 Axios Authorization 头
    delete instance.defaults.headers.common['Authorization'];
    // 清除 Cookie token
    removeToken();
    // 清除 Store 用户信息
    yield put(setUserInfo(initialState));
  } else {
    // 设置 Axios Authorization 头
    instance.defaults.headers.common['Authorization'] = `Bearer ${token}`;
    // 设置 Cookie token
    if (!action.payload.refresh) {
      setToken(token, action.payload.rememberMe);
    }
    // 获取并记录用户信息到 Store
    try {
      const result: BasicSuccessResult<GetUserInfoResponse> =
        yield call(api.auth.getUserInfo, {
          data: { token },
        });
      yield put(setUserInfo(toLowerCamelCase(result.data)));
    } catch (error) {
      handleFailure(error);
    }
  }
}

// watcher Saga
function* watcher(): SagaIterator {
  yield takeEvery(setUserToken.type, getUserInfoAsync);
}

// root Saga
export default watcher;
