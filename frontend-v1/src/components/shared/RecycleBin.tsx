import { useState } from 'react';
import { Alert, Button, Drawer, List, message } from 'antd';
import { useIntl } from 'react-intl';
import { request } from '@/apis';

interface RecycledItem {
  id: string;
  name: string;
  group_type: 'team' | 'project' | 'file' | 'project_set';
  deleted_at: number;
  expires_at?: number;
}

const TYPE_MESSAGE_ID: Record<RecycledItem['group_type'], string> = {
  team: 'recycle.type.team',
  project: 'recycle.type.project',
  project_set: 'recycle.type.projectSet',
  file: 'recycle.type.file',
};

export function RecycleBin({
  teamID,
  onRestore,
}: {
  teamID?: string;
  onRestore?: () => void;
}) {
  const { formatMessage } = useIntl();
  const [visible, setVisible] = useState(false);
  const [loading, setLoading] = useState(false);
  const [items, setItems] = useState<RecycledItem[]>([]);
  const refresh = async () => {
    setLoading(true);
    try {
      setItems(
        (
          await request<RecycledItem[]>({
            method: 'GET',
            url: teamID
              ? `/v1/teams/${teamID}/recycle-bin`
              : '/v1/user/recycle-bin',
            params: { paging: false },
          })
        ).data,
      );
    } catch {
      message.error(formatMessage({ id: 'recycle.loadFailed' }));
    } finally {
      setLoading(false);
    }
  };
  return (
    <>
      <Button
        onClick={() => {
          setVisible(true);
          void refresh();
        }}
      >
        {formatMessage({ id: 'recycle.bin' })}
      </Button>
      <Drawer
        title={formatMessage({ id: 'recycle.bin' })}
        visible={visible}
        width={480}
        onClose={() => setVisible(false)}
      >
        <Alert type="info" message={formatMessage({ id: 'recycle.tip' })} />
        <List
          loading={loading}
          dataSource={items}
          renderItem={(item) => {
            const expires = item.expires_at || item.deleted_at + 7 * 86400;
            return (
              <List.Item
                actions={[
                  <Button
                    key="restore"
                    disabled={expires <= Date.now() / 1000}
                    onClick={async () => {
                      try {
                        const collection =
                          item.group_type === 'team'
                            ? 'teams'
                            : item.group_type === 'project'
                              ? 'projects'
                              : item.group_type === 'project_set'
                                ? 'project-sets'
                                : 'files';
                        await request({
                          method: 'POST',
                          url: `/v1/${collection}/${item.id}/restore`,
                        });
                        onRestore?.();
                        window.location.reload();
                      } catch {
                        message.error(
                          formatMessage({ id: 'recycle.restoreFailed' }),
                        );
                      }
                    }}
                  >
                    {formatMessage({ id: 'recycle.restore' })}
                  </Button>,
                ]}
              >
                <List.Item.Meta
                  title={item.name}
                  description={formatMessage(
                    { id: 'recycle.expires' },
                    {
                      type: formatMessage({ id: TYPE_MESSAGE_ID[item.group_type] }),
                      time: new Date(expires * 1000).toLocaleString(),
                    },
                  )}
                />
              </List.Item>
            );
          }}
        />
      </Drawer>
    </>
  );
}
