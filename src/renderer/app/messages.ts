/** Chinese and English strings of the window chrome and the in-app directory browser. */
import { defineMessages } from '../i18n.ts'

const zh = {
  appMenu: '应用菜单',
  fileMenu: '文件',
  editMenu: '编辑',
  viewMenu: '视图',
  helpMenu: '帮助',
  settings: '设置',
  browserTitle: '选择工作区目录',
  browserNewFolder: '新建文件夹',
  browserFolderName: '文件夹名称',
  browserCreateIn: '在“{name}”中新建文件夹',
  browserUntitledFolder: '未命名文件夹',
  browserCreate: '创建',
  browserCancel: '取消',
  browserOpen: '打开',
  browserEditPath: '编辑路径',
  browserLoading: '加载中…',
  browserShowHidden: '显示隐藏文件',
  browserLocations: '位置',
}

const en: Readonly<Record<keyof typeof zh, string>> = {
  appMenu: 'Application menu',
  fileMenu: 'File',
  editMenu: 'Edit',
  viewMenu: 'View',
  helpMenu: 'Help',
  settings: 'Settings',
  browserTitle: 'Select Workspace Directory',
  browserNewFolder: 'New folder',
  browserFolderName: 'Folder name',
  browserCreateIn: 'New folder in “{name}”',
  browserUntitledFolder: 'Untitled folder',
  browserCreate: 'Create',
  browserCancel: 'Cancel',
  browserOpen: 'Open',
  browserEditPath: 'Edit path',
  browserLoading: 'Loading…',
  browserShowHidden: 'Show hidden files',
  browserLocations: 'Locations',
}

/** Window chrome strings. */
export const appMessages = defineMessages(zh, en)
/** @returns The window chrome translate function for the current locale. */
export const useAppT = appMessages.useT
