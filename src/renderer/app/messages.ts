/** Chinese and English strings of the page-wide failure notices (`feedback.ts`). */
import { defineMessages } from '../i18n.ts'

const zh = {
  copyFailed: '复制失败：系统剪贴板拒绝了写入，请点击窗口后重试。',
  connectionLost: '与 Rainy 的连接已断开，这次操作没有完成。',
}

const en: Readonly<Record<keyof typeof zh, string>> = {
  copyFailed: 'Copy failed: the system clipboard refused the text. Click the window and try again.',
  connectionLost: 'The connection to Rainy was lost, so this action did not finish.',
}

/** Failure notice strings. */
export const appMessages = defineMessages(zh, en)
