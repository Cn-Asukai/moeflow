import type { Rule } from 'antd/lib/form';
import { getIntl } from '@/locales';
import { isValidPassword } from '@/utils/password';

export const passwordRules: Rule[] = [
  { required: true },
  {
    validator: (_rule, value?: string) =>
      !value || isValidPassword(value)
        ? Promise.resolve()
        : Promise.reject(getIntl().formatMessage({ id: 'auth.passwordLength' })),
  },
];
