// 导出进度
export enum OUTPUT_STATUS {
  QUEUING = 0, // 排队中
  RUNNING = 1, // 进行中
  SUCCEEDED = 2, // 完成
  ERROR = 3, // 失败或取消
}
export const OUTPUT_STATUS_LABELS: Record<OUTPUT_STATUS, string> = {
  [OUTPUT_STATUS.QUEUING]: 'Queued',
  [OUTPUT_STATUS.RUNNING]: 'Running',
  [OUTPUT_STATUS.SUCCEEDED]: 'Download',
  [OUTPUT_STATUS.ERROR]: 'Failed or cancelled',
};
// 导出类型
export enum OUTPUT_TYPE {
  ALL = 0, // 所有内容
  ONLY_TEXT = 1, // 仅文本
}
