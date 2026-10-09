// @vitest-environment jsdom
/** Restored legacy guide tabs delegate to their existing file navigation. */
import { afterEach, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/react'
import type { ComponentProps } from 'react'
import type { PaneId, TabId } from '@deepseek-ai/dsh-client-ui-dockkit'
import { WorkspaceStart } from '../src/client/WorkspaceStart.tsx'
import { GeneralSettingsBridge } from '../src/client/GeneralSettingsBridge.tsx'
import { globalProps } from './global-props.client.ts'

afterEach(cleanup)

it('opens existing appearance settings from the unified settings subpage and releases its listener', () => {
  const openSettings = vi.fn()
  const view = render(<GeneralSettingsBridge {...globalProps} wide={false} settingsOpen={false}
    openSettings={openSettings} openOnboarding={vi.fn()} />)
  window.dispatchEvent(new CustomEvent('rainy:open-general-settings'))
  expect(openSettings).toHaveBeenCalledOnce()
  expect(view.container.textContent).toBe('')
  view.unmount()
  window.dispatchEvent(new CustomEvent('rainy:open-general-settings'))
  expect(openSettings).toHaveBeenCalledOnce()
})

it('replaces a visible restored guide with files without adding another tab', () => {
  const openTab = vi.fn()
  let visible = false
  const props: ComponentProps<typeof WorkspaceStart> = { useTabInfo: () => ({
    sidebar: { expanded: true, fullscreen: false }, panel: { id: 'pane1' as PaneId },
    tab: { id: 'tab2' as TabId, kind: 'guide', contentId: 'sidebar://guide', title: 'Start', visible,
      signal: new AbortController().signal, navigation: { address: 'sidebar://guide', params: undefined, revision: 0 },
      actions: { openTab, bindCommands: vi.fn(() => vi.fn()), openResource: vi.fn(), close: vi.fn() } },
  }) }
  const view = render(<WorkspaceStart {...props} />)
  expect(openTab).not.toHaveBeenCalled()
  visible = true
  view.rerender(<WorkspaceStart {...props} />)
  expect(openTab).toHaveBeenCalledExactlyOnceWith('files', { replaceTab: true })
  expect(view.container.textContent).toBe('')
})
