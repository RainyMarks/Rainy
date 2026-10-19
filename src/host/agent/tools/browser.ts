/**
 * `browser_open`, `browser_read`, `browser_act` and `browser_screenshot`: the agent looks at the application it is
 * building and checks it by itself, instead of asking the user whether it works.
 *
 * The page runs in a hidden window the carrier owns (`src/main/preview-browser.ts`), which only opens loopback
 * addresses — the project's own development server.
 */
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import type { ImageContent, TextContent } from '@earendil-works/pi-ai'
import { limitOutput, textResult, type ToolContext } from './common.ts'

/** What the carrier answers; every field is optional, because a failed command still reports what it can. */
export interface PreviewAnswer {
  url?: string | undefined
  title?: string | undefined
  text?: string | undefined
  elements?: string[] | undefined
  console?: string[] | undefined
  failed?: string[] | undefined
  image?: string | undefined
  error?: string | undefined
}

/** Sends one command to the carrier's preview window. */
export type PreviewAccess = (command: {
  action: 'open' | 'read' | 'click' | 'type' | 'press' | 'screenshot' | 'close'
  url?: string | undefined
  target?: string | undefined
  text?: string | undefined
  key?: string | undefined
}) => Promise<PreviewAnswer>

/** What the card shows for a browser call. */
export interface BrowserDetails {
  url?: string | undefined
  title?: string | undefined
  errors: number
}

/**
 * The page as the model reads it: where it is, what it says, what can be clicked, and what went wrong.
 * @param answer The carrier's answer.
 * @returns Model-facing text.
 */
export function renderPage(answer: PreviewAnswer): string {
  const parts: string[] = []
  if (answer.error !== undefined) parts.push(`[${answer.error}]`)
  if (answer.url !== undefined) parts.push(`URL: ${answer.url}${answer.title === undefined || answer.title === '' ? '' : `\nTitle: ${answer.title}`}`)
  const failed = answer.failed ?? []
  const logged = answer.console ?? []
  if (failed.length > 0) parts.push(`Requests that failed (${failed.length}):\n${failed.join('\n')}`)
  if (logged.length > 0) parts.push(`Console (${logged.length}):\n${logged.join('\n')}`)
  if (logged.length === 0 && failed.length === 0 && answer.url !== undefined) parts.push('Console: no errors or warnings.')
  if (answer.elements !== undefined && answer.elements.length > 0) parts.push(`Elements:\n${answer.elements.join('\n')}`)
  if (answer.text !== undefined && answer.text !== '') parts.push(`Page text:\n${answer.text}`)
  else if (answer.url !== undefined) parts.push('Page text: (the page shows no text)')
  return parts.join('\n\n')
}

function details(answer: PreviewAnswer): BrowserDetails {
  return { url: answer.url, title: answer.title, errors: (answer.failed?.length ?? 0) + (answer.console?.filter(line => line.startsWith('error')).length ?? 0) }
}

const openParameters = Type.Object({
  url: Type.String({ description: 'Address of the running application on this machine, such as http://localhost:5173/. Only loopback addresses open.' }),
})
const readParameters = Type.Object({})
const actParameters = Type.Object({
  action: Type.Union([Type.Literal('click'), Type.Literal('type'), Type.Literal('press')], {
    description: 'click an element, type text into a field, or press a key in it (such as Enter).',
  }),
  target: Type.String({ description: 'The element: its visible text or label as browser_read listed it, or a CSS selector.' }),
  text: Type.Optional(Type.String({ description: 'Text to type, for action "type"; the key to press, for action "press" (default Enter).' })),
})
const screenshotParameters = Type.Object({})

/**
 * The browser tools, when the carrier can show a preview.
 * @param context Chat tool context, for the output limit.
 * @param preview Sends commands to the carrier.
 * @param vision The chat's model accepts images, so the screenshot tool is worth offering.
 * @returns Tool definitions.
 */
export function browserTools(context: ToolContext, preview: PreviewAccess, vision: boolean): AgentTool[] {
  const open: AgentTool<typeof openParameters, BrowserDetails> = {
    name: 'browser_open',
    label: 'Open preview',
    description: 'Open the application you are building in a preview window and report what the page shows: its text, the elements that can be '
      + 'clicked or typed into, and the console errors and failed requests. Use it to check your own work after starting a development server, '
      + 'instead of asking the user whether the page works. Only addresses on this machine (localhost, 127.0.0.1) open.',
    parameters: openParameters,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof openParameters>) {
      const answer = await preview({ action: 'open', url: params.url })
      const result = textResult(await limitOutput(context, 'browser', renderPage(answer)), details(answer))
      return answer.error !== undefined && answer.url === undefined ? { ...result, isError: true } : result
    },
  }
  const read: AgentTool<typeof readParameters, BrowserDetails> = {
    name: 'browser_read',
    label: 'Read preview',
    description: 'Read the preview page again: its text, its elements, and the console errors and failed requests since it was opened. '
      + 'Use it after the application reloads itself or after waiting for something to finish.',
    parameters: readParameters,
    executionMode: 'sequential',
    async execute() {
      const answer = await preview({ action: 'read' })
      const result = textResult(await limitOutput(context, 'browser', renderPage(answer)), details(answer))
      return answer.error !== undefined && answer.url === undefined ? { ...result, isError: true } : result
    },
  }
  const act: AgentTool<typeof actParameters, BrowserDetails> = {
    name: 'browser_act',
    label: 'Use preview',
    description: 'Click an element, type into a field or press a key in the preview page, then report the page again. '
      + 'Name the element by the visible text browser_read listed, or by a CSS selector.',
    parameters: actParameters,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof actParameters>) {
      if (params.target.trim() === '') throw new Error('invalid target: name the element by its visible text or a CSS selector')
      const answer = await preview({
        action: params.action, target: params.target,
        ...(params.action === 'press' ? { key: params.text ?? 'Enter' } : { text: params.text ?? '' }),
      })
      const result = textResult(await limitOutput(context, 'browser', renderPage(answer)), details(answer))
      return answer.error !== undefined ? { ...result, isError: true } : result
    },
  }
  const screenshot: AgentTool<typeof screenshotParameters, BrowserDetails> = {
    name: 'browser_screenshot',
    label: 'Preview screenshot',
    description: 'Take a picture of the preview page, for what text cannot tell you: layout, spacing, colors and overlap.',
    parameters: screenshotParameters,
    executionMode: 'sequential',
    async execute() {
      const answer = await preview({ action: 'screenshot' })
      if (answer.image === undefined) {
        return { ...textResult(renderPage(answer), details(answer)), isError: true }
      }
      const content: (TextContent | ImageContent)[] = [
        { type: 'text', text: `Screenshot of ${answer.url ?? 'the preview'}` },
        { type: 'image', data: answer.image, mimeType: 'image/png' },
      ]
      return { content, details: details(answer) }
    },
  }
  return [open, read, act, ...(vision ? [screenshot] : [])] as AgentTool[]
}
