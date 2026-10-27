import { createTranslator } from '@onlyne/node-host/i18n';

const en = {
  'ack.received': 'Received',
} as const;

const zh: Record<keyof typeof en, string> = {
  'ack.received': '已收到',
};

export type LarkTextKey = keyof typeof en;

export const t = createTranslator({ en, 'zh-CN': zh });
