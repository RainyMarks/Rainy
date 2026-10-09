/** Tool discovery, per-tool downloads and native launch controls in the CTF workbench. */
import { useState } from 'react'
import { Button, Input, Pill, Tag, Tooltip, fileSizeText, IconSearchOutlineRegular, IconRefreshOutlineRegular,
  IconPinOutlineRegular, IconPinFillRegular, IconDownloadOutlineRegular, IconTrashOutlineRegular } from '../ui/index.ts'
import { nativeToolIds } from '../../shared/native-tools-protocol.ts'
import type { NativeToolId, NativeToolSummary, NativeToolsOperation } from '../../shared/native-tools-protocol.ts'
import type { NativeToolsState } from './native-tools.ts'
import { useCtfT, type CtfMessageKey } from './messages.ts'
import css from './ToolCatalog.module.css'
import { toolGroup, toolGroups, type ToolGroup } from './tool-groups.ts'

const purpose: Record<typeof nativeToolIds[number], CtfMessageKey> = {
  yakit: 'toolYakit', cyberchef: 'toolCyberChef', '7zip': 'tool7zip', exiftool: 'toolExiftool',
  wireshark: 'toolWireshark', binwalk: 'toolBinwalk', ffmpeg: 'toolFfmpeg', audacity: 'toolAudacity',
  stegsolve: 'toolStegsolve', pngcheck: 'toolPngcheck', qrazybox: 'toolQrazybox', 'image-lsb-viewer': 'toolImageLsb',
  imagemagick: 'toolImagemagick', ida: 'toolIda', x64dbg: 'toolX64dbg', die: 'toolDie', imhex: 'toolImhex',
  jadx: 'toolJadx', dnspy: 'toolDnspy', 'pyinstxtractor-ng': 'toolPyinstxtractor',
  winmerge: 'toolWinMerge', qalculate: 'toolQalculate', 'sonic-visualiser': 'toolSonicVisualiser',
  'tesseract-ocr': 'toolTesseract', sox: 'toolSox', tweakpng: 'toolTweakPng', 'multimon-ng': 'toolMultimon',
  'gnu-strings': 'toolStrings', gimp: 'toolGimp', curl: 'toolCurl', jq: 'toolJq', yq: 'toolYq',
  sqlite: 'toolSqlite', 'sqlite-browser': 'toolSqliteBrowser', qpdf: 'toolQpdf', ripgrep: 'toolRipgrep',
  bruno: 'toolBruno', pcapfix: 'toolPcapfix',
}
const kindCopy = { desktop: 'toolsDesktop', terminal: 'toolsTerminal', web: 'toolsOfflineWeb' } as const
type Filter = 'all' | 'installed' | 'favorites' | 'recent' | ToolGroup
const views: readonly { readonly id: Filter; readonly copyKey: CtfMessageKey }[] = [
  { id: 'all', copyKey: 'toolsAll' }, { id: 'installed', copyKey: 'toolsInstalled' },
  { id: 'favorites', copyKey: 'toolsFavorites' }, { id: 'recent', copyKey: 'toolsRecent' },
]
const groupCopy = Object.fromEntries(toolGroups.map(group => [group.id, group.copyKey])) as Record<ToolGroup, CtfMessageKey>
const sum = (tools: readonly NativeToolSummary[]): number => tools.reduce((total, tool) => total + tool.downloadBytes, 0)

/** Actions supplied by the catalog's owner. */
export interface ToolCatalogActions {
  readonly checkToolUpdates: () => Promise<void>
  readonly operateTools: (operation: NativeToolsOperation, ids?: readonly NativeToolId[]) => Promise<void>
  readonly cancelDownload: () => Promise<void>
  readonly loadTools: () => Promise<void>
  readonly launchTool: (id: NativeToolId, variant?: 'x32') => Promise<void>
  readonly toggleFavorite: (id: NativeToolId) => Promise<void>
}

/**
 * Filter the directory, download single tools on demand and open the installed ones.
 * @param props Tool availability and native operations.
 * @returns The catalog, or the desktop availability notice in a browser.
 */
export function ToolCatalog({ state, loadTools, launchTool, toggleFavorite, operateTools, cancelDownload, checkToolUpdates }:
  ToolCatalogActions & { readonly state: NativeToolsState }) {
  const t = useCtfT()
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<Filter>('all')
  const [removing, setRemoving] = useState<NativeToolId | undefined>()
  const { download } = state
  const operating = download.phase === 'verifying' || download.phase === 'downloading' || download.phase === 'installing'
  const percentage = download.totalBytes > 0 ? Math.min(100, Math.floor(download.completedBytes * 100 / download.totalBytes)) : 0
  const installed = state.tools.filter(tool => tool.status !== 'available')
  const available = state.tools.filter(tool => tool.status === 'available')
  const outdated = state.tools.filter(tool => tool.outdated)
  const updatable = installed.length > 0 && (outdated.length > 0 || state.catalogOutdated)
  const description = (id: NativeToolId): string => {
    const known = nativeToolIds.find(value => value === id)
    return t(known === undefined ? 'toolExtra' : purpose[known])
  }
  const progress = t(download.phase === 'verifying' ? 'toolsProgressVerify' : download.phase === 'downloading' ? 'toolsProgressDownload'
    : download.operation === 'remove' ? 'toolsProgressRemove' : 'toolsProgressInstall', { progress: String(percentage) })
  const search = query.trim().toLocaleLowerCase()
  const favoriteIds = state.preferences.favorites
  const recentIds = state.preferences.recent
  const tools = state.tools.filter((tool) => {
    if (filter === 'installed' && tool.status === 'available') return false
    if (filter === 'favorites' && !favoriteIds.includes(tool.id)) return false
    if (filter === 'recent' && !recentIds.includes(tool.id)) return false
    if (!views.some(view => view.id === filter) && toolGroup(tool) !== filter) return false
    return search === '' || `${tool.name} ${tool.id} ${description(tool.id)}`.toLocaleLowerCase().includes(search)
  })
  if (filter === 'recent') tools.sort((a, b) => recentIds.indexOf(a.id) - recentIds.indexOf(b.id))
  // The complete unfiltered list reads by task; filtered views stay flat and tag each tool with its group.
  const grouped = filter === 'all' && search === ''
  const filters = [...views, ...toolGroups.filter(group => state.tools.some(tool => toolGroup(tool) === group.id))]
  if (state.phase === 'desktop-only') return <div className={css.empty} data-rainy-tool-catalog>
    <p>{t('toolsDesktopOnly')}</p><p className={css.secondary}>{t('toolsDesktopHint')}</p>
  </div>
  const busy = state.phase === 'loading' || state.savingFavorites || state.pending.length > 0
  const locked = operating || state.phase === 'loading'
  const card = (tool: NativeToolSummary, showGroup: boolean) => {
    const favorite = favoriteIds.includes(tool.id)
    const pending = state.pending.includes(tool.id)
    const favoriteLabel = t(favorite ? 'toolsRemoveFavorite' : 'toolsAddFavorite', { name: tool.name })
    const working = operating && (download.tools?.includes(tool.id) ?? false)
    return <li key={tool.id} className={css.card} data-tool-id={tool.id}>
      <div className={css.details}>
        <div className={css.nameRow}><h3>{tool.name}</h3>{showGroup && <Tag>{t(groupCopy[toolGroup(tool)])}</Tag>}</div>
        <p className={css.purpose}>{description(tool.id)}</p>
        <div className={css.metadata}>
          <span>{tool.version || t('toolsVersionUnknown')}</span><span>{t(kindCopy[tool.launchKind])}</span>
          <Tag tone={tool.status === 'missing' ? 'warning' : tool.status === 'ready' && tool.verified ? 'success' : 'neutral'}>
            {t(tool.status === 'available' ? 'toolsNotInstalled' : tool.status === 'missing' ? 'toolsMissing' : tool.verified ? 'toolsReady' : 'toolsUnverified')}
          </Tag>
          {tool.outdated && <Tag tone="warning">{t('toolsOutdated')}</Tag>}
          {working && <span role="status">{progress}</span>}
        </div>
        {tool.missing.length > 0 && <p className={css.missing}>{tool.missing.join(' · ')}</p>}
        {(tool.status === 'missing' || tool.variants?.some(variant => variant.status === 'missing')) && <div className={css.repair}>
          <span>{t('toolsRepairHint')}</span>
          <Button size="sm" variant="outline" icon={<IconRefreshOutlineRegular />} disabled={locked}
            onClick={() => { void operateTools('repair') }}>{t('toolsRepair')}</Button>
        </div>}
      </div>
      <div className={css.actions}>
        <Tooltip label={favoriteLabel} portal>
          <Button size="sm" aria-label={favoriteLabel} aria-pressed={favorite} disabled={state.savingFavorites || state.phase === 'loading' || operating}
            icon={favorite ? <IconPinFillRegular /> : <IconPinOutlineRegular />} onClick={() => { void toggleFavorite(tool.id) }} />
        </Tooltip>
        {tool.status === 'available' ? <Button size="sm" variant="outline" icon={<IconDownloadOutlineRegular />}
          aria-label={t('toolsDownloadName', { name: tool.name })} disabled={locked} aria-busy={working}
          onClick={() => { void operateTools('install', [tool.id]) }}>{t('toolsDownload', { size: fileSizeText(tool.downloadBytes) })}</Button> : <>
          <Button size="sm" variant="outline" aria-label={t('toolsOpenName', { name: tool.name })}
            disabled={tool.status === 'missing' || pending || locked} aria-busy={pending}
            onClick={() => { void launchTool(tool.id) }}>
            {t(pending ? 'toolsOpening' : tool.launchKind === 'terminal' ? 'toolsOpenTerminal' : 'toolsOpen')}
          </Button>
          {tool.variants?.map(variant => <Button key={variant.id} size="sm" variant="outline"
            aria-label={t('toolsOpenName', { name: variant.name })} disabled={variant.status !== 'ready' || pending || locked}
            onClick={() => { void launchTool(tool.id, variant.id) }}>{variant.name}</Button>)}
          {removing === tool.id ? <Button size="sm" variant="outline" disabled={locked || pending}
            onClick={() => { setRemoving(undefined); void operateTools('remove', [tool.id]) }}>{t('toolsRemoveConfirm', { name: tool.name })}</Button>
            : <Tooltip label={t('toolsRemoveName', { name: tool.name })} portal>
              <Button size="sm" aria-label={t('toolsRemoveName', { name: tool.name })} disabled={locked || pending}
                icon={<IconTrashOutlineRegular />} onClick={() => { setRemoving(tool.id) }} />
            </Tooltip>}
        </>}
      </div>
    </li>
  }
  return <div className={css.catalog} data-rainy-tool-catalog>
    <div className={css.toolbar}>
      <div className={css.search}><Input icon={<IconSearchOutlineRegular />} aria-label={t('toolsSearch')}
        placeholder={t('toolsSearch')} value={query} onChange={(event) => { setQuery(event.target.value) }} /></div>
      <Tooltip label={t('toolsRefresh')} portal>
        <Button size="sm" aria-label={t('toolsRefresh')} icon={<IconRefreshOutlineRegular />} disabled={busy}
          onClick={() => { void loadTools() }} />
      </Tooltip>
    </div>
    <div className={css.notice}>
      <div><span>{t('toolsSummary', { installed: String(installed.length), total: String(state.tools.length) })}</span>
        {operating && <span role="status">{progress}</span>}
        {download.phase === 'cancelled' && <small>{t('toolsDownloadCancelled')}</small>}
        {download.phase === 'error' && <small role="alert">{download.error}</small>}
        {state.update.phase === 'available' && <small>{t('toolsUpdateAvailable', { version: state.update.version })}</small>}
        {state.update.phase === 'current' && <small>{t('toolsUpdateCurrent')}</small>}
        {state.update.phase === 'error' && <small role="alert">{state.update.error}</small>}
      </div>
      <div className={css.actions}>
        {operating ? <Button size="sm" variant="outline" onClick={() => { void cancelDownload() }}>{t('toolsDownloadCancel')}</Button> : <>
          {updatable && <Button size="sm" variant="primary" disabled={busy}
            onClick={() => { void operateTools('update') }}>{t('toolsUpdateAll', { size: fileSizeText(state.updateBytes) })}</Button>}
          {available.length > 0 && <Button size="sm" variant="outline" disabled={busy}
            onClick={() => { void operateTools('install', available.map(tool => tool.id)) }}>
            {t('toolsDownloadAll', { size: fileSizeText(sum(available)) })}
          </Button>}
        </>}
        <Button size="sm" variant="outline" disabled={operating || state.update.phase === 'checking'}
          onClick={() => { void checkToolUpdates() }}>{t(state.update.phase === 'checking' ? 'toolsUpdateChecking' : 'toolsUpdateCheck')}</Button>
      </div>
      {operating && <progress className={css.downloadProgress} aria-label={progress} value={percentage} max={100} />}
    </div>
    <div className={css.filters} role="group" aria-label={t('toolsCategories')}>
      {filters.map(item => <Pill key={item.id} active={filter === item.id} aria-pressed={filter === item.id}
        onClick={() => { setFilter(item.id) }}>{t(item.copyKey)}</Pill>)}
    </div>
    {state.phase === 'error' && <div className={css.notice} role="alert">
      <span>{t('toolsLoadFailed')}{state.error && <small>{state.error}</small>}</span>
      <Button size="sm" onClick={() => { void loadTools() }}>{t('ctfRetry')}</Button>
    </div>}
    {state.phase === 'loading' && state.tools.length === 0 ? <div className={css.skeletons} role="status" aria-label={t('toolsLoading')}>
      {[0, 1, 2, 3].map(id => <div key={id} className={css.skeleton} aria-hidden="true"><span /><span /></div>)}
    </div> : <>
      <p className={css.count} role="status">{t('toolsCount', { count: String(tools.length) })}</p>
      {tools.length === 0 ? <div className={css.empty}><p>{t(search !== '' ? 'toolsEmpty'
        : filter === 'favorites' ? 'toolsEmptyFavorites' : filter === 'recent' ? 'toolsEmptyRecent' : 'toolsEmpty')}</p></div>
        : grouped ? toolGroups.map((group) => {
          const members = tools.filter(tool => toolGroup(tool) === group.id)
          return members.length === 0 ? null : <section key={group.id} className={css.group} aria-label={t(group.copyKey)}>
            <h2 className={css.groupTitle}>{t(group.copyKey)}<span>{members.length}</span></h2>
            <ul className={css.list}>{members.map(tool => card(tool, false))}</ul>
          </section>
        })
          : <ul className={css.list} aria-label={t('toolsCatalog')}>{tools.map(tool => card(tool, true))}</ul>}
    </>}
  </div>
}
