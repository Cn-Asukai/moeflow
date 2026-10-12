/**
 * 项目相关 API
 */
import { AxiosRequestConfig } from 'axios';
import { PaginationParams, request } from '.';
import {
  IMPORT_FROM_LABELPLUS_ERROR_TYPE,
  IMPORT_FROM_LABELPLUS_STATUS,
  PROJECT_STATUS,
} from '../constants';
import { Language, Project, ProjectSet, Role, Team } from '../interfaces';
import { toUnderScoreCase } from '../utils';
import { directUpload } from './upload';
import { waitForJob } from './jobs';

export interface APIProject {
  groupType: 'project';
  id: string;
  name: string;
  intro: string;
  hasAvatar: boolean;
  avatar: string;
  allowApplyType: number;
  isNeedCheckApplication: boolean;
  maxUser: number;
  userCount: number;
  status: number;
  createTime: string;
  editTime: string;
  role: Role;
  autoBecomeProjectAdmin: boolean;
  sourceLanguage: Language;
  targetCount: number;
  sourceCount: number;
  translatedSourceCount: number;
  checkedSourceCount: number;
  team: Team;
  projectSet: ProjectSet;
  importFromLabelplusStatus: IMPORT_FROM_LABELPLUS_STATUS;
  importFromLabelplusPercent: number;
  importFromLabelplusErrorType: IMPORT_FROM_LABELPLUS_ERROR_TYPE;
  importFromLabelplusErrorTypeName: string;
}

/** 获取团队的项目列表的请求数据 */
interface GetTeamProjectsParams {
  word?: string;
  status?: PROJECT_STATUS;
}
/** 获取团队的项目列表 */
const getTeamProjects = ({
  teamID,
  projectSetID,
  params,
  configs,
}: {
  teamID: string;
  projectSetID: string;
  params?: GetTeamProjectsParams & PaginationParams;
  configs?: AxiosRequestConfig;
}) => {
  return request<Project[]>({
    method: 'GET',
    url: `/v1/teams/${teamID}/projects`,
    params: { project_set: projectSetID, ...toUnderScoreCase(params) },
    ...configs,
  });
};

/** 获取用户的项目列表的请求数据 */
interface GetUserProjectsParams {
  word?: string;
  status?: PROJECT_STATUS;
}
/** 获取用户的项目列表 */
const getUserProjects = ({
  params,
  configs,
}: {
  params?: GetUserProjectsParams & PaginationParams;
  configs?: AxiosRequestConfig;
}) => {
  return request<Project[]>({
    method: 'GET',
    url: `/v1/user/projects`,
    params: { ...toUnderScoreCase(params) },
    ...configs,
  });
};

/** 获取项目 */
const getProject = ({
  id,
  configs,
}: {
  id: string;
  configs?: AxiosRequestConfig;
}) => {
  return request<Project>({
    method: 'GET',
    url: `/v1/projects/${id}`,
    ...configs,
  });
};

/** 新建项目的请求数据 */
interface CreateProjectData {
  name: string;
  intro: string;
  allowApplyType: number;
  applicationCheckType: number;
  defaultRole: string;
  labelplusTXT?: string;
}
/** 新建项目 */
const createProject = async ({ teamID, data, configs }: { teamID: string; data: CreateProjectData; configs?: AxiosRequestConfig }) => {
  const { labelplusTXT, ...metadata } = data;
  const result = await request<{ project: Project; message: string }>({ method: 'POST', url: `/v1/teams/${teamID}/projects`, data: toUnderScoreCase(metadata), ...configs });
  if (labelplusTXT !== undefined) {
    const uploaded = await directUpload<{ resource_id: string }>(new Blob([labelplusTXT], { type: 'text/plain' }), { purpose: 'import', team_id: teamID, name: 'translations.txt' }, configs);
    const task = await request<{ task_id: string }>({ method: 'POST', url: `/v1/teams/${teamID}/imports`, data: { resource_id: uploaded.data.resource_id, project_id: result.data.project.id, format: 'labelplus' } });
    await waitForJob(task.data.task_id);
    result.data.project = (await getProject({ id: result.data.project.id })).data;
  }
  return result;
};

/** 导入项目的请求数据 */
interface ImportProjectData { project: Blob }
/** Import an entire archive through a durable backend task. */
const importProject = async ({ teamID, projectSetID, data, configs }: {
  teamID: string; projectSetID: string; data: ImportProjectData; configs?: AxiosRequestConfig;
}) => {
  const archive = await directUpload<{ resource_id: string }>(data.project, { purpose: 'import', team_id: teamID, name: 'project.zip' }, configs);
  return request<{ task_id: string }>({ method: 'POST', url: `/v1/teams/${teamID}/imports`, data: { resource_id: archive.data.resource_id, project_set_id: projectSetID, format: 'zip' }, ...configs });
};

/** 上传文件 */
const uploadFile = ({
  projectID,
  filename,
  file,
  configs,
}: {
  projectID: string;
  filename: string;
  file: Blob;
  configs?: AxiosRequestConfig;
}) => {
  return directUpload(file, { purpose: 'file', project_id: projectID, name: filename }, configs);
};

/** 修改项目的请求数据 */
interface EditProjectData {
  name: string;
  intro: string;
  allowApplyType: number;
  applicationCheckType: number;
  defaultRole: string;
}
/** 修改项目 */
const editProject = ({
  id,
  data,
  configs,
}: {
  id: string;
  data: EditProjectData;
  configs?: AxiosRequestConfig;
}) => {
  return request<{ project: Project; message: string }>({
    method: 'PUT',
    url: `/v1/projects/${id}`,
    data: toUnderScoreCase(data),
    ...configs,
  });
};

/** 完结项目 */
const finishProject = ({
  id,
  configs,
}: {
  id: string;
  configs?: AxiosRequestConfig;
}) => {
  return request<{ project: Project; message: string }>({
    method: 'POST',
    url: `/v1/projects/${id}/finish`,
    ...configs,
  });
};

/**
 * @deprecated being retired
 */
const startProjectOCR = ({
  id,
  configs,
}: {
  id: string;
  configs?: AxiosRequestConfig;
}) => {
  return request({
    method: 'POST',
    url: `/v1/projects/${id}/ocr`,
    ...configs,
  });
};

export default {
  getUserProjects,
  getTeamProjects,
  getProject,
  createProject,
  editProject,
  finishProject,
  startProjectOCR,
  importProject,
  uploadFile,
};
