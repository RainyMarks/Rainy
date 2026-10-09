/** Rainy's first-message consumer of the shared composer surface and normal submission controllers. */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-api-workspace-controller/client'
import type {} from '@deepseek-ai/dsh-client-ui-conversation/client'
import type {} from '@deepseek-ai/dsh-client-ui-session/client'
import type {} from '@deepseek-ai/dsh-client-ui-workspace/client'
import type { HostObservable, InjectFace, PropsRenderFactories, PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type { IdeModel, IdeState } from './ide-model.ts'
import { DeferredDraft, publishPreparedDraft, type DeferredDraftState } from './deferred-draft.ts'
import { IconFolderCloseRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import css from './IdeShell.module.css'

interface Injected {
  readonly draft: DeferredDraft
  readonly hooks: { readonly ide: HostObservable<IdeState>; readonly deferred: HostObservable<DeferredDraftState> }
}

function ProjectContext({ useIde }: PropsRuntime<'conversation.hero.project'> & InjectFace<Injected>) {
  const workspace = useIde(state => state.workspace)
  if (workspace === null) return null
  return <div className={css.chatProject} title={workspace.path}>
    <IconFolderCloseRegular size={16} /><span>{workspace.title}</span>
  </div>
}

function Composer({ draft, useIde, useDeferred, renderFactorySlot }:
  PropsRuntime<'conversation.composer.bar'> & InjectFace<Injected> & PropsRenderFactories) {
  const workspace = useIde(state => state.workspace)
  const phase = useIde(state => state.phase)
  const state = useDeferred(value => value)
  return renderFactorySlot('conversation.deferredComposer', {
    // The hero project context already names the project above this composer.
    ...state, disabled: workspace === null || phase !== 'ready', workspace: '',
    onChange: (text) => { draft.change(text) }, onAddFiles: (files) => { draft.addFiles(files) },
    onRemoveFile: (index) => { draft.removeFile(index) }, onSubmit: () => { void draft.submit() },
  })
}

/**
 * Mount a browser-only composer while no main Session is selected.
 * @param ctx - Rainy client context with normal Workspace, Session and Conversation services.
 * @param model - independently selected IDE project.
 */
export function installDeferredComposer(ctx: Context, model: IdeModel): void {
  const t = ctx.locale.bind('rainy')
  const draft = new DeferredDraft({
    workspace: () => {
      const state = model.state.getSnapshot()
      return state.phase === 'ready' ? state.workspace?.workspaceId ?? null : null
    },
    handoff: async (workspaceId, contents, adopt, lifetime) => {
      const navigation = ctx.layout.beginNavigation()
      const project = new AbortController()
      const cancelOnProjectChange = (): void => {
        const state = model.state.getSnapshot()
        if (state.phase !== 'ready' || state.workspace?.workspaceId !== workspaceId) project.abort()
      }
      const stop = model.state.subscribe(cancelOnProjectChange)
      const assertCurrent = (): void => {
        lifetime.throwIfAborted()
        project.signal.throwIfAborted()
        navigation.throwIfAborted()
      }
      try {
        cancelOnProjectChange()
        assertCurrent()
        const sessionId = await ctx.sessions.create({ workspaceId })
        assertCurrent()
        await ctx.sessions.using(sessionId, { source: 'controllerOperation' }, async (reference) => {
          await reference.ready
          assertCurrent()
          const scope = ctx.sessions.scope(sessionId)
          const conversation = scope?.get('conversation')
          if (scope === undefined || conversation === undefined) throw new Error(t('ideSessionUnavailable'))
          const prepared = conversation.prepareDraft(contents.text, contents.files)
          const cancellation = AbortSignal.any([lifetime, project.signal, navigation])
          cancellation.addEventListener('abort', prepared.cancel, { once: true })
          if (cancellation.aborted) prepared.cancel()
          const ready = prepared.ready.finally(() => { cancellation.removeEventListener('abort', prepared.cancel) })
          await publishPreparedDraft(ready, {
            assertCurrent,
            reveal: () => { ctx.uiWorkspace.openSession(sessionId); adopt() },
            submit: () => { conversation.input.for(scope).submit() },
            fail: (error) => {
              conversation.input.for(scope).notify('error', error instanceof Error ? error.message : String(error))
            },
          })
        })
      } finally { stop() }
    },
  })
  ctx.effect(() => () => draft.dispose())
  ctx.slots.inject('conversation.hero.project', () => ctx.slots.register({
    name: 'conversation.hero.project',
    inject: (): Injected => ({ draft, hooks: { ide: model.state, deferred: draft.state } }),
  }, ProjectContext))
  ctx.on('workspace/resolve-new', workspaceId => async () => {
    if (workspaceId === undefined) { ctx.uiWorkspace.clearSession(); return }
    const workspace = ctx.workspaces.list.getSnapshot().items.find(item => item.workspaceId === workspaceId)
    if (workspace === undefined) throw new Error(t('ideSessionUnavailable'))
    await model.openWorkspace(workspace.path)
    if (model.state.getSnapshot().workspace?.workspaceId === workspaceId) ctx.uiWorkspace.clearSession()
  })
  ctx.on('workspace/resolve-open', (workspaceId, signal) => {
    if (ctx.uiSession.adapter.current.getSnapshot().key !== undefined) return undefined
    return async () => {
      signal.throwIfAborted()
      const workspace = ctx.workspaces.list.getSnapshot().items.find(item => item.workspaceId === workspaceId)
      if (workspace === undefined) throw new Error(t('ideSessionUnavailable'))
      await model.openWorkspace(workspace.path)
    }
  })
  ctx.slots.inject('conversation.composer.bar', () => {
    let withdraw: (() => void) | undefined
    const sync = (): void => {
      const pending = ctx.uiSession.adapter.current.getSnapshot().key === undefined
      if (pending && withdraw === undefined) withdraw = ctx.slots.register({
        name: 'conversation.composer.bar', priority: -1,
        inject: (): Injected => ({ draft, hooks: { ide: model.state, deferred: draft.state } }),
      }, Composer)
      else if (!pending && withdraw !== undefined) { withdraw(); withdraw = undefined }
    }
    const stop = ctx.uiSession.adapter.current.subscribe(sync)
    sync()
    return () => { stop(); withdraw?.() }
  })
}
