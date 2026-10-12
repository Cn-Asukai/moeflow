import { useState } from 'react';
import { Button, Drawer, List, message, Alert } from 'antd';
import { request } from '@/apis';
interface RecycledItem { id: string; name: string; group_type: 'team' | 'project' | 'file' | 'project_set'; deleted_at: number; expires_at?: number }
export function RecycleBin({ teamID, onRestore }: { teamID?: string; onRestore?: () => void }) {
  const [visible, setVisible] = useState(false);
  const [loading, setLoading] = useState(false);
  const [items, setItems] = useState<RecycledItem[]>([]);
  const refresh = async () => {
    setLoading(true);
    try { setItems((await request<RecycledItem[]>({ method: 'GET', url: teamID ? `/v1/teams/${teamID}/recycle-bin` : '/v1/user/recycle-bin', params: { paging: false } })).data); }
    catch (error) { message.error('Unable to load recycle bin'); }
    finally { setLoading(false); }
  };
  return <><Button onClick={() => { setVisible(true); void refresh(); }}>Recycle bin</Button><Drawer title="Recycle bin" visible={visible} width={480} onClose={() => setVisible(false)}><Alert type="info" message="Deleted items and their history can be restored for 7 days. After 7 days they are permanently deleted. Restore a deleted parent first." /><List loading={loading} dataSource={items} renderItem={(item) => {
    const expires = item.expires_at || item.deleted_at + 7 * 86400;
    return <List.Item actions={[<Button key="restore" disabled={expires <= Date.now() / 1000} onClick={async () => { try { await request({ method: 'POST', url: `/v1/${item.group_type === 'team' ? 'teams' : item.group_type === 'project' ? 'projects' : item.group_type === 'project_set' ? 'project-sets' : 'files'}/${item.id}/restore` }); onRestore?.(); window.location.reload(); } catch (error) { message.error('Restore failed: parent unavailable, expired, or permission denied'); } }}>Restore</Button>]}><List.Item.Meta title={item.name} description={`${item.group_type} · expires ${new Date(expires * 1000).toLocaleString()}`} /></List.Item>;
  }} /></Drawer></>;
}
