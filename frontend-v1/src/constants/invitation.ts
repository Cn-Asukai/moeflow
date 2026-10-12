// 邀请加入状态
export const INVITATION_STATUS = {
  PENDING: 0 as const,
  ALLOW: 1 as const,
  DENY: 2 as const,
  WITHDRAWN: 3 as const,
};
export type InvitationStatuses =
  (typeof INVITATION_STATUS)[keyof typeof INVITATION_STATUS];
