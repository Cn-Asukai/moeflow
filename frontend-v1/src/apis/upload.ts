import axios, { AxiosRequestConfig } from 'axios';
import { BasicSuccessResult, request } from '.';
import store from '@/store';
interface UploadTicket { id: string; upload_url?: string; headers?: Record<string, string>; expires_at: number; session_expires_at: number; upload_mode?: 'multipart' | 'single'; part_size?: number }
interface UploadSession { id: string; upload_mode: 'multipart' | 'single'; part_size?: number; session_expires_at: number; uploaded: boolean }
interface UploadedPart { part_number: number; etag: string; size: number }
export interface DirectUploadOptions { purpose: 'file' | 'user_avatar' | 'team_avatar' | 'import'; project_id?: string; team_id?: string; parent_id?: string; name: string }
/** COS bytes never pass through the API. Failed transfers/confirmation reuse the same upload intent until binding succeeds. */
export async function directUpload<T = Record<string, unknown>>(file: Blob, options: DirectUploadOptions, configs: AxiosRequestConfig = {}): Promise<BasicSuccessResult<T>> {
  const contentType = /\.(txt|md|srt)$/i.test(options.name) ? 'text/plain' : /\.zip$/i.test(options.name) ? 'application/zip' : file.type || (/\.png$/i.test(options.name) ? 'image/png' : /\.jpe?g$/i.test(options.name) ? 'image/jpeg' : /\.webp$/i.test(options.name) ? 'image/webp' : /\.gif$/i.test(options.name) ? 'image/gif' : 'application/octet-stream');
  const hashes: string[] = [];
  for (let offset = 0; offset < file.size; offset += 8 * 1024 * 1024) {
    if (configs.signal?.aborted) throw new Error('Upload paused');
    const hash = await crypto.subtle.digest('SHA-256', await file.slice(offset, offset + 8 * 1024 * 1024).arrayBuffer());
    hashes.push(Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, '0')).join(''));
  }
  const key = `cos-upload:${store.getState().user.id}:${JSON.stringify(options)}:${file.size}:${hashes.join('')}`;
  const apiConfig = { signal: configs.signal, cancelToken: configs.cancelToken };
  let session: UploadSession | undefined;
  let ticket: UploadTicket | undefined;
  const stored = localStorage.getItem(key);
  if (stored) {
    try {
      const saved: unknown = JSON.parse(stored);
      if (saved && typeof saved === 'object' && 'id' in saved && typeof saved.id === 'string' && 'session_expires_at' in saved && typeof saved.session_expires_at === 'number' && saved.session_expires_at > Date.now() / 1000 && 'upload_mode' in saved && (saved.upload_mode === 'single' || saved.upload_mode === 'multipart') && 'uploaded' in saved && typeof saved.uploaded === 'boolean') {
        session = { id: saved.id, session_expires_at: saved.session_expires_at, upload_mode: saved.upload_mode, uploaded: saved.uploaded, part_size: 'part_size' in saved && typeof saved.part_size === 'number' ? saved.part_size : undefined };
      } else localStorage.removeItem(key);
    } catch { localStorage.removeItem(key); }
  }
  if (!session) {
    ticket = (await request<UploadTicket>({ method: 'POST', url: '/v1/uploads', data: { ...options, size: file.size, content_type: contentType }, ...apiConfig })).data;
    session = { id: ticket.id, upload_mode: ticket.upload_mode || 'single', part_size: ticket.part_size, session_expires_at: ticket.session_expires_at, uploaded: false };
    localStorage.setItem(key, JSON.stringify(session));
  }
  if (!session.uploaded) {
    if (session.upload_mode === 'multipart') {
      const existing = (await request<{ parts: UploadedPart[]; part_size: number }>({ method: 'GET', url: `/v1/uploads/${session.id}/parts`, ...apiConfig })).data;
      const partSize = existing.part_size;
      let loaded = existing.parts.reduce((sum, part) => sum + part.size, 0);
      for (let offset = 0, number = 1; offset < file.size; offset += partSize, number++) {
        if (existing.parts.some((part) => part.part_number === number)) continue;
        const part = file.slice(offset, Math.min(file.size, offset + partSize));
        const signed = (await request<{ upload_url: string; headers: Record<string, string> }>({ method: 'POST', url: `/v1/uploads/${session.id}/parts`, data: { part_number: number }, ...apiConfig })).data;
        await axios.put(signed.upload_url, part, { headers: signed.headers, ...apiConfig, onUploadProgress: (event) => configs.onUploadProgress?.({ ...event, loaded: loaded + event.loaded, total: file.size, progress: (loaded + event.loaded) / file.size }) });
        loaded += part.size;
      }
    } else {
      if (!ticket) ticket = (await request<UploadTicket>({ method: 'POST', url: `/v1/uploads/${session.id}/sign`, ...apiConfig })).data;
      if (!ticket.upload_url) throw new Error('Upload URL missing');
      await axios.put(ticket.upload_url, file, { headers: ticket.headers, ...apiConfig, onUploadProgress: configs.onUploadProgress });
    }
    session.uploaded = true;
    localStorage.setItem(key, JSON.stringify(session));
  }
  await request({ method: 'POST', url: `/v1/uploads/${session.id}/complete`, ...apiConfig });
  const result = await request<T>({ method: 'POST', url: `/v1/uploads/${session.id}/bind`, ...apiConfig });
  localStorage.removeItem(key);
  return result;
}
