import { useState } from 'react';
import { Button, Drawer, List, message } from 'antd';
import { request } from '@/apis';
interface Revision { id: string; name: string; revision: number; activated: boolean; size?: number; created_at?: string }
export function FileHistory({ fileID }: { fileID: string }) {
  const [open, setOpen] = useState(false);
  const [items, setItems] = useState<Revision[]>([]);
  const refresh = async () => { try { setItems((await request<Revision[]>({ method: 'GET', url: `/v1/files/${fileID}/history`, params: { paging: false } })).data); } catch { message.error('History unavailable'); } };
  return <span onClick={(event) => event.stopPropagation()}><Button size="small" onClick={() => { setOpen(true); void refresh(); }}>History</Button><Drawer title="File history" visible={open} onClose={() => setOpen(false)} width={440}><p>Text revisions retain their sources and translations. Activating a revision selects it as the current file.</p><List dataSource={items} renderItem={(item) => <List.Item actions={[<Button key="activate" disabled={item.activated} onClick={async () => { try { await request({ method: 'POST', url: `/v1/files/${item.id}/activate` }); await refresh(); window.location.reload(); } catch { message.error('Cannot activate revision: permission denied or project read-only'); } }}>{item.activated ? 'Current' : 'Activate'}</Button>]}><List.Item.Meta title={`${item.name} · revision ${item.revision}`} description={item.created_at} /></List.Item>} /></Drawer></span>;
}
