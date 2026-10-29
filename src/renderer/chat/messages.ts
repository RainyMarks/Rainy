/** Strings of the Markdown renderer the viewer pages share: the code block toolbar and images. */
import { defineMessages } from '../i18n.ts'

export const { useT: useChatT } = defineMessages({
  copy: '复制',
  copied: '已复制',
  remoteImage: '远程图片未加载：{host}', remoteImageOpen: '在浏览器中打开', localImage: '图片：{path}',
  wrap: '自动换行',
  unwrap: '取消换行',
  code: '代码',
}, {
  copy: 'Copy',
  copied: 'Copied',
  remoteImage: 'Remote image not loaded: {host}', remoteImageOpen: 'Open in browser', localImage: 'Image: {path}',
  wrap: 'Wrap lines',
  unwrap: 'Unwrap lines',
  code: 'Code',
})
