import { useState } from 'react';
import { Button, Drawer, List, message } from 'antd';
import { useIntl } from 'react-intl';
import { request } from '@/apis';

interface Revision {
  id: string;
  name: string;
  revision: number;
  activated: boolean;
  size?: number;
  created_at?: string;
}

export function FileHistory({ fileID }: { fileID: string }) {
  const { formatMessage } = useIntl();
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<Revision[]>([]);
  const refresh = async () => {
    try {
      setItems(
        (
          await request<Revision[]>({
            method: 'GET',
            url: `/v1/files/${fileID}/history`,
            params: { paging: false },
          })
        ).data,
      );
    } catch {
      message.error(formatMessage({ id: 'file.historyUnavailable' }));
    }
  };
  return (
    <span onClick={(event) => event.stopPropagation()}>
      <Button
        size="small"
        onClick={() => {
          setOpen(true);
          void refresh();
        }}
      >
        {formatMessage({ id: 'file.history' })}
      </Button>
      <Drawer
        title={formatMessage({ id: 'file.historyTitle' })}
        visible={open}
        onClose={() => setOpen(false)}
        width={440}
      >
        <p>{formatMessage({ id: 'file.historyTip' })}</p>
        <List
          dataSource={items}
          renderItem={(item) => (
            <List.Item
              actions={[
                <Button
                  key="activate"
                  disabled={item.activated}
                  onClick={async () => {
                    try {
                      await request({
                        method: 'POST',
                        url: `/v1/files/${item.id}/activate`,
                      });
                      await refresh();
                      window.location.reload();
                    } catch {
                      message.error(
                        formatMessage({ id: 'file.historyActivateFailed' }),
                      );
                    }
                  }}
                >
                  {item.activated
                    ? formatMessage({ id: 'file.historyCurrent' })
                    : formatMessage({ id: 'file.historyActivate' })}
                </Button>,
              ]}
            >
              <List.Item.Meta
                title={formatMessage(
                  { id: 'file.historyRevision' },
                  { name: item.name, revision: item.revision },
                )}
                description={item.created_at}
              />
            </List.Item>
          )}
        />
      </Drawer>
    </span>
  );
}
