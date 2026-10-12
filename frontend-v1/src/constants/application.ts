// 申请加入状态
export const APPLICATION_STATUS = {
  PENDING: 0 as const,
  ALLOW: 1 as const,
  DENY: 2 as const,
  WITHDRAWN: 3 as const,
};
export type ApplicationStatuses =
  (typeof APPLICATION_STATUS)[keyof typeof APPLICATION_STATUS];
