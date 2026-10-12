import { Modal } from 'antd';
import { File as MFile, Target } from '@/interfaces';
import { ModalStaticFunctions } from 'antd/lib/modal/confirm';

import { BatchTranslateModalContent } from './BatchTranslateModal';
import { useMemo } from 'react';
import { IntlShape, useIntl } from 'react-intl';


export type ModalHandle = ReturnType<typeof Modal.confirm>;

interface TranslationCallbacks {
  onFileSaved?(f: MFile): void;
}

interface TranslatorApi {
  start(callbacks: TranslationCallbacks): Promise<void>;
}
function bind(
  files: MFile[],
  target: Target,
  modal: ModalStaticFunctions,
  { formatMessage }: IntlShape,
): TranslatorApi {
  return {
    start,
  };
  async function start(callbacks: TranslationCallbacks) {

    await new Promise<boolean>((resolve) => {
      const handle = modal.confirm({
        icon: null,
        title: formatMessage({ id: 'fileList.aiTranslate.startTranslate' }),
        content: (
          <BatchTranslateModalContent
            files={files}
            target={target}
            onFileSaved={callbacks.onFileSaved}
            getHandle={() => handle as ModalHandle}
          />
        ),
        okButtonProps: { disabled: true },
        onOk: () => {
          resolve(true);
        },
        onCancel: () => {
          resolve(false);
        },
      });
    });
  }
}

export function useAiTranslate(
  files: MFile[],
  target: Target,
): [true, TranslatorApi, React.ReactNode] | [false, null, null] {
  const [modal, contextHolder] = Modal.useModal();
  const intl = useIntl();

  const api = useMemo(
    () => bind(files, target, modal as ModalStaticFunctions, intl),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [target.id, files.map((file) => file.id).join('|')],
  );

  return [true, api, contextHolder];
}
