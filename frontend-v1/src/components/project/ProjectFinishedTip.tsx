import React from 'react';
import { useIntl } from 'react-intl';
import { FC } from '../../interfaces';

/** 项目完结提示的属性接口 */
interface ProjectFinishedTipProps {
  className?: string;
}
/**
 * 项目完结提示
 */
export const ProjectFinishedTip: FC<ProjectFinishedTipProps> = ({
  className,
}) => {
  const { formatMessage } = useIntl(); // i18n

  return <div role="status" className={className} style={{ padding: '12px 16px', width: '100%' }}>{formatMessage({ id: 'project.finishedTip' })}</div>;
};
