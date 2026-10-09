/** Renderer entry. */
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import 'katex/dist/katex.min.css'
import './theme/tokens.css'
import './theme/base.css'
import './theme/highlight.css'
import './theme/app.css'
import './prefs.ts'
import { App } from './app/App.tsx'

createRoot(document.getElementById('root')!).render(<StrictMode><App /></StrictMode>)
