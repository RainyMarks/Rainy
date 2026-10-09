/** Localized native editing and application information menus. */
import type { MenuItemConstructorOptions } from 'electron'

const labels = {
  zh: { undo: '撤销', redo: '重做', cut: '剪切', copy: '复制', paste: '粘贴', selectAll: '全选', about: '关于 RainyAgent', updates: '检查更新…' },
  en: { undo: 'Undo', redo: 'Redo', cut: 'Cut', copy: 'Copy', paste: 'Paste', selectAll: 'Select all', about: 'About RainyAgent', updates: 'Check for updates…' },
}

/** Resolve a native menu with Electron-owned edit roles.
 * @param menu Requested menu.
 * @param locale Current product language.
 * @param about Open application information.
 * @param checkUpdates Check for a newer stable application release.
 * @returns Native menu entries.
 */
export function nativeMenuTemplate(menu: 'edit' | 'help', locale: 'zh' | 'en', about: () => void, checkUpdates: () => void): MenuItemConstructorOptions[] {
  const text = labels[locale]
  if (menu === 'help') return [{ label: text.updates, click: checkUpdates }, { type: 'separator' }, { label: text.about, click: about }]
  return [
    { role: 'undo', label: text.undo }, { role: 'redo', label: text.redo }, { type: 'separator' },
    { role: 'cut', label: text.cut }, { role: 'copy', label: text.copy }, { role: 'paste', label: text.paste },
    { type: 'separator' }, { role: 'selectAll', label: text.selectAll },
  ]
}
