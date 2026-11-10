import { useTitle } from '@/hooks';
import { useHistory } from 'react-router-dom';
import { FC, useEffect } from 'react';
import { useIntl } from 'react-intl';

export const NotFoundPage: FC = () => {
  const history = useHistory();
  const { formatMessage } = useIntl();
  useTitle({
    prefix: formatMessage({ id: 'site.notFoundTitle' }),
  });
  useEffect(() => {
    const timer = setTimeout(() => {
      history.push('/');
    }, 3000);

    return () => {
      clearTimeout(timer);
    };
  }, [history]);
  return <div>{formatMessage({ id: 'site.notFound' })}</div>;
};
