import { Project, Team } from '@/interfaces';
import { PROJECT_STATUS } from '@/constants/project';

/**
 * 测试用户是否有某些权限
 * @param group
 * @param permission
 */
export const can = (
  group: Team | Project | undefined,
  permission: number,
): boolean => {
  if (group?.groupType === 'project' && group.status === PROJECT_STATUS.FINISHED && ![1, 5, 1010, 1060].includes(permission)) return false;
  if (group && group.role) {
    return group.role.permissions.findIndex((p) => p.id === permission) > -1;
  }
  return false;
};
