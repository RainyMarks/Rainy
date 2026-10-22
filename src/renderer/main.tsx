/** Renderer entry. */
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import '@fontsource-variable/hanken-grotesk'
import '@fontsource-variable/geist-mono'
import './theme/tokens.css'
import './theme/schemes.css'
import './theme/surfaces.css'
import './theme/base.css'
import './theme/highlight.css'
import './theme/app.css'
import './prefs.ts'
import { App } from './app/App.tsx'
import { installFeedback } from './app/feedback.ts'
import { ViewerApp } from './viewer/ViewerApp.tsx'

installFeedback()
// The native workbench shows single files in WebView2 panes at `/?view=…`.
const viewer = new URLSearchParams(location.search).has('view')
createRoot(document.getElementById('root')!).render(<StrictMode>{viewer ? <ViewerApp /> : <App />}</StrictMode>)
