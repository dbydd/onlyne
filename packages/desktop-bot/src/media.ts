import { lstat, mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { statSync } from 'node:fs';
import { extname, isAbsolute, join, resolve } from 'node:path';
import type { MediaItem, MediaKind } from './contracts.ts';
import { shortId } from './ids.ts';
import { runtimeWorkspace } from './runtime-config.ts';

const LIMIT = 25 * 1024 * 1024;
const TYPES: Record<string, { kind: MediaKind; contentType: string }> = {
  '.png': { kind: 'image', contentType: 'image/png' },
  '.jpg': { kind: 'image', contentType: 'image/jpeg' },
  '.jpeg': { kind: 'image', contentType: 'image/jpeg' },
  '.gif': { kind: 'image', contentType: 'image/gif' },
  '.webp': { kind: 'image', contentType: 'image/webp' },
  '.avif': { kind: 'image', contentType: 'image/avif' },
  '.svg': { kind: 'svg', contentType: 'image/svg+xml' },
  '.mp4': { kind: 'video', contentType: 'video/mp4' },
  '.webm': { kind: 'video', contentType: 'video/webm' },
  '.mov': { kind: 'video', contentType: 'video/quicktime' },
  '.mp3': { kind: 'audio', contentType: 'audio/mpeg' },
  '.ogg': { kind: 'audio', contentType: 'audio/ogg' },
  '.wav': { kind: 'audio', contentType: 'audio/wav' },
  '.flac': { kind: 'audio', contentType: 'audio/flac' },
};

export function mediaKind(contentType: string, name: string): { kind: MediaKind; contentType: string } {
  const known = TYPES[extname(name).toLowerCase()];
  if (known !== undefined) return known;
  const lower = contentType.toLowerCase();
  if (lower.startsWith('image/svg')) return { kind: 'svg', contentType: 'image/svg+xml' };
  if (lower.startsWith('image/')) return { kind: 'image', contentType: lower };
  if (lower.startsWith('video/')) return { kind: 'video', contentType: lower };
  if (lower.startsWith('audio/')) return { kind: 'audio', contentType: lower };
  return { kind: 'file', contentType: lower.length > 0 ? lower : 'application/octet-stream' };
}

export interface MediaService {
  put(bytes: Uint8Array, options: { name: string; contentType: string; botId?: string; kind?: MediaKind }): Promise<MediaItem>;
  item(id: string): MediaItem | undefined;
  path(item: MediaItem): string;
  fromPath(path: string, options: { botId?: string; name?: string; kind?: MediaKind }): Promise<MediaItem>;
  list(): MediaItem[];
}

export function createMediaService(dataDir: string, index: () => MediaItem[], commit: (item: MediaItem) => Promise<void>): MediaService {
  const root = join(dataDir, 'media');
  const service: MediaService = {
    item(id) { return index().find(item => item.id === id); },
    list() { return [...index()]; },
    path(item) { return join(root, item.id); },
    async put(bytes, options) {
      if (bytes.length === 0) throw new Error('The media file is empty');
      if (bytes.length > LIMIT) throw new Error('The media file is larger than 25 MB');
      const sniffed = mediaKind(options.contentType, options.name);
      const id = shortId('m');
      await mkdir(root, { recursive: true });
      const temporary = join(root, `${id}.tmp`);
      await writeFile(temporary, bytes, { mode: 0o600 });
      await rename(temporary, join(root, id));
      const item: MediaItem = {
        id, kind: options.kind ?? sniffed.kind, contentType: sniffed.contentType, name: options.name,
        size: bytes.length, createdAt: Date.now(), ...(options.botId === undefined ? {} : { botId: options.botId }),
      };
      await commit(item);
      return item;
    },
    async fromPath(path, options) {
      const info = await lstat(path).catch(() => undefined);
      if (info === undefined || !info.isFile()) throw new Error(`Media file not found: ${path}`);
      if (info.size > LIMIT) throw new Error('The media file is larger than 25 MB');
      const bytes = new Uint8Array(await readFile(path));
      const item = await service.put(bytes, {
        name: options.name ?? path.slice(path.lastIndexOf('/') + 1), contentType: mediaKind('', path).contentType,
        ...(options.botId === undefined ? {} : { botId: options.botId }), ...(options.kind === undefined ? {} : { kind: options.kind }),
      });
      return item;
    },
  };
  return service;
}

/** Resolves a model- or user-supplied path against the bot workspace and the runtime workspace. */
export async function resolveMediaPath(dataDir: string, botId: string, conversationId: string, workspace: string, path: string): Promise<string> {
  if (isAbsolute(path)) return path;
  const candidates = [resolve(workspace, path), resolve(runtimeWorkspace(dataDir, botId, conversationId), path), resolve(dataDir, 'bots', botId, '.omp', path)];
  for (const candidate of candidates) {
    const info = await lstat(candidate).catch(() => undefined);
    if (info?.isFile()) return candidate;
  }
  throw new Error(`Media path not found: ${path}`);
}

export function stickerPath(dataDir: string, botId: string, name: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(name)) throw new Error('A sticker name uses letters, digits, underscores or hyphens');
  const base = join(dataDir, 'bots', botId, '.omp', 'stickers', name);
  for (const extension of ['.png', '.gif', '.webp', '.jpg', '.jpeg']) {
    const candidate = `${base}${extension}`;
    try {
      const info = statSync(candidate);
      if (info.isFile()) return candidate;
    } catch { /* try the next extension */ }
  }
  throw new Error(`No sticker named ${name} in ${join(dataDir, 'bots', botId, '.omp', 'stickers')}`);
}
