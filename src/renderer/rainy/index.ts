/** Rainy brand contributions for the existing DSH sidebar and conversation slots. */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-ui-renderer/client'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar/client'
import type {} from '@deepseek-ai/dsh-client-ui-conversation/client'
import type {} from '@deepseek-ai/dsh-client-ui-layout/client'
import type {} from '@deepseek-ai/dsh-client-ui-settings/client'
import type {} from '@deepseek-ai/dsh-client-locale/client'
import type { SidebarBrandMarkOwnerProps } from '@deepseek-ai/dsh-client-ui-sidebar/client'
import { createElement } from 'react'
import { installFiles } from './files.ts'
import { installCtfWorkbench } from './ctf.tsx'
import { installIde } from './ide.tsx'
import { installSnippetActions } from './SnippetActions.tsx'
import { installDeferredComposer } from './DeferredComposer.tsx'
import { installRainySettings } from './settings.tsx'
import { Config } from '../config.ts'
import type {} from '@deepseek-ai/dsh-client-ui-session/client'
import { zh, en } from './locales.ts'
import { WorkspaceStart } from './WorkspaceStart.tsx'
import css from './IdeShell.module.css'

export const inject = ['slots', 'sidebarRightTabs', 'sidebarRight', 'sidebarRightApplications', 'layout', 'locale', 'theme', 'uiSession', 'sessions', 'workspaces', 'uiWorkspace', 'shortcuts', 'conversation']

/** Render the installed product image at the geometry supplied by the owning slot. */
export function RainyMark({ size }: SidebarBrandMarkOwnerProps) {
  return createElement('img', { src: '/rainy/icon.png', width: size, height: size, alt: '', className: css.mark })
}
/** Product name remains the same in all supported locales. */
export function RainyName() { return createElement('span', null, 'RainyAgent') }

/** Registers reversible workspace contributions. @param ctx - Rainy Client context. */
export function apply(ctx: Context): void {
  const configuration = (globalThis as { __RAINY_WORKBENCH_CONFIG__?: unknown }).__RAINY_WORKBENCH_CONFIG__
  if (configuration === undefined) throw new Error('Rainy workbench configuration was not supplied by the Host.')
  const config = Config(configuration)
  ctx.effect(() => ctx.locale.register('rainy', { zh, en }))
  const editor = installIde(ctx, config)
  installRainySettings(ctx, editor, config)
  installDeferredComposer(ctx, editor)
  installSnippetActions(ctx, editor)
  installFiles(ctx, editor)
  installCtfWorkbench(ctx, config, editor)
  ctx.slots.inject('sidebar.right.tab.guide', () => ctx.slots.register({
    name: 'sidebar.right.tab.guide', select: () => ({}),
  }, WorkspaceStart))
  ctx.effect(() => {
    document.documentElement.dataset.rainyUi = ''
    document.documentElement.dataset.shellRail = 'external'
    return () => {
      delete document.documentElement.dataset.rainyUi
      delete document.documentElement.dataset.shellRail
    }
  })
  ctx.slots.inject('sidebar.brand.mark', () => ctx.slots.register({ name: 'sidebar.brand.mark' }, RainyMark))
  ctx.slots.inject('sidebar.brand.name', () => ctx.slots.register({ name: 'sidebar.brand.name' }, RainyName))
  ctx.slots.inject('conversation.hero.brand.mark', () => ctx.slots.register({ name: 'conversation.hero.brand.mark' }, RainyMark))
}
