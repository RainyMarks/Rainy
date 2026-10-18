/** `show_widget`: an interactive answer — a small page of HTML, CSS and JavaScript the chat shows in a sandboxed frame
 *  while the model writes it. The guidance below is adapted from CopilotKit's Open Intelligent UI (MIT). */
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { WIDGET_MAX_CHARS, WIDGET_TOOL } from '../../../shared/widget-protocol.ts'
import { textResult } from './common.ts'

const parameters = Type.Object({
  title: Type.String({ description: 'Short title of the widget in the user\'s language, such as "Loan calculator".' }),
  height: Type.Number({ description: 'Estimated finished height in pixels (40–2400); the frame then follows the content.' }),
  css: Type.String({ description: 'All styles, written before the markup. The frame stays a placeholder until this field is complete, so keep it lean.' }),
  html: Type.String({ description: 'Body markup (no <html>, <head> or <body> tags, no <style>). It is shown while it streams, so put readable content first.' }),
  script: Type.Optional(Type.String({ description: 'JavaScript run once after the markup is complete, as a classic script. Omit for a static widget.' })),
})

const DESCRIPTION = `Show the user an interactive answer: a small web page rendered inline in the chat while you write it. \
Use it when seeing or manipulating something explains better than prose — a chart of data, a diagram of a mechanism, a calculator, \
a comparison with adjustable weights, a simulation with sliders, a step-through of an algorithm, a 3D model. \
Answer in plain text instead for facts, writing, code changes and short explanations; do not call it just to decorate an answer.

Write the arguments in this exact order: title, height, css, html, script. The user watches it stream: \
styles first, then markup (shown as it arrives), then the script, which runs once everything is complete.

Environment:
- A sandboxed frame without network access: no fetch, XHR, WebSocket, remote images, fonts or scripts, no localStorage, \
cookies or parent DOM. Put any data the widget needs directly into the script, and state where it came from.
- Theme variables follow the user's light or dark theme; always use them instead of fixed colors: \
--bg, --surface, --surface-2, --text, --text-2, --text-3, --border, --border-strong, --accent, --accent-text, \
--success, --warning, --danger, --control, --control-hover, --code-bg, --font, --font-mono, --radius, \
and the series colors --chart-1 … --chart-8. Canvas code reads them with rainy.color('--accent'); the window fires \
"rainythemechange" when the theme changes.
- Built-in styles already cover typography, buttons (class "primary" for the main action), inputs, selects, range sliders, \
tables and the helper classes card, row, stack, grid, stat (with .value and .label), badge, muted, hint, num, error, ok. \
Only write css for layout and what is specific to this widget.
- Libraries load on use: Chart.js as the global Chart, d3 as the global d3, and Three.js as an ES module: \
put \`<script type="module">import * as THREE from 'three'; import { OrbitControls } from 'three/addons/controls/OrbitControls.js'; …</script>\` \
in the markup instead of using the script field. Prefer plain HTML and inline SVG when they explain as well.
- rainy.sendPrompt(text) sends text as the user's next chat message; call it only from a button the user clicks, labeled \
as sending a message, with the current selections in the text, and disable the button until the promise settles. \
rainy.openLink(url) opens an https page in the browser; plain <a href> links do the same.

Quality:
- Every control must work: connect handlers, compute correct results, and give sensible defaults. Validate inputs \
(empty, non-finite, out of range, division by zero) and show a readable message instead of NaN or Infinity.
- State units, assumptions and formulas; label sample or illustrative data as such inside the widget. Never invent \
live prices, weather or statistics; ask, or use clearly labeled examples.
- The frame width is the chat column (often 360–700 px): let layouts wrap, give SVG a viewBox with width 100%, size canvases \
to their container, and never use vh units or fixed heights for the page. Support keyboard use and labeled inputs.
- Write all visible text in the user's language. Use textContent, not innerHTML, for text built from input.
- After the call, add at most a sentence or two of text; do not repeat what the widget shows. A changed request gets a new \
widget; earlier widgets cannot be modified.`

/**
 * The `show_widget` tool. The chat renders the widget from the call's arguments; executing the call only checks them.
 * @returns Tool definition.
 */
export function widgetTool(): AgentTool<typeof parameters> {
  return {
    name: WIDGET_TOOL,
    label: 'Interactive answer',
    description: DESCRIPTION,
    parameters,
    async execute(_id, params: Static<typeof parameters>) {
      if (params.html.trim() === '') throw new Error('invalid html: the widget needs markup')
      const size = params.css.length + params.html.length + (params.script?.length ?? 0)
      if (size > WIDGET_MAX_CHARS) throw new Error(`the widget is ${size} characters; keep css, html and script under ${WIDGET_MAX_CHARS} together`)
      return textResult('The widget is shown to the user. Its scripts run in their browser; you cannot see the result or errors unless the user reports them.', undefined)
    },
  }
}
