/**
 * 各种类型选项（系统角色/加入审核类型等） API
 */
import { request } from '.';
import { toUnderScoreCase, toHyphenCase } from '@/utils';
import { AxiosRequestConfig } from 'axios';
import { Role } from '@/interfaces';

export type TypeNames =
  | 'allowApplyType'
  | 'applicationCheckType'
  | 'systemRole';
export type GroupTypes = 'team' | 'project';
export interface APITypeOption { id: number; name: string; intro?: string }
export type APISystemRole = Omit<Role, 'systemCode' | 'createTime'> & { system_code: string; create_time?: string };

/** 获取系统角色的请求数据 */
interface GetSystemRolesData {
  typeName: TypeNames;
  groupType: GroupTypes;
  params?: { with_creator?: boolean };
  configs?: AxiosRequestConfig;
}
/** 获取系统角色 */
const getTypes = <Name extends TypeNames>({ typeName, groupType, params, configs }: Omit<GetSystemRolesData, 'typeName'> & { typeName: Name }) => {
  return request<Name extends 'systemRole' ? APISystemRole[] : APITypeOption[]>({
    method: 'GET',
    url: `/v1/types/${toHyphenCase(typeName)}`,
    params: {
      group_type: groupType,
      ...toUnderScoreCase(params),
    },
    ...configs,
  });
};

export default {
  getTypes,
};
