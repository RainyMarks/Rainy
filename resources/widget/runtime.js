/* Rainy interactive answer frame runtime. Runs inside a sandboxed frame with an opaque origin: it shows the styles and
 * markup the chat streams in, runs the answer's scripts once it is complete, reports its height and errors, and offers
 * `window.rainy` for the two things a widget may ask of the page: send a chat message after a click, and open a link. */
(function () {
  'use strict'
  var TAG = 'rainy-widget'
  var LIB = '/rainy/widget/lib/'
  /* Libraries offered as globals, loaded when a script mentions them. */
  var GLOBALS = [
    { name: 'Chart', file: 'chart.umd.min.js', pattern: /\bChart\b/ },
    { name: 'd3', file: 'd3.min.js', pattern: /\bd3\s*\./ },
  ]
  var host = window.parent
  var finished = false
  var errors = 0
  var nextCall = 1
  var calls = {}
  var themeStyle = document.createElement('style')
  var answerStyle = document.createElement('style')
  document.head.appendChild(themeStyle)
  document.head.appendChild(answerStyle)

  function post(message) {
    message.tag = TAG
    host.postMessage(message, '*')
  }

  function call(type, fields) {
    return new Promise(function (resolve, reject) {
      var id = nextCall++
      calls[id] = { resolve: resolve, reject: reject }
      fields.type = type
      fields.id = id
      post(fields)
    })
  }

  function report(message) {
    if (errors >= 5) return
    errors++
    post({ type: 'error', message: String(message).slice(0, 500) })
  }

  window.addEventListener('error', function (event) {
    var where = event.lineno ? ' (line ' + event.lineno + ')' : ''
    report((event.message || 'Script error') + where)
  })
  window.addEventListener('unhandledrejection', function (event) {
    var reason = event.reason
    report(reason && reason.message ? reason.message : String(reason))
  })

  window.rainy = Object.freeze({
    /* Send `text` as the user's next chat message. Only works right after the user clicks in the widget. */
    sendPrompt: function (text) {
      if (typeof text !== 'string' || text.trim() === '') return Promise.reject(new Error('sendPrompt needs text'))
      return call('prompt', { text: text })
    },
    /* Open an http(s) address in the browser. */
    openLink: function (url) {
      return call('link', { url: String(url) })
    },
    /* The current value of a theme variable, such as rainy.color('--accent'), for canvas drawing. */
    color: function (name) {
      return getComputedStyle(document.documentElement).getPropertyValue(name).trim()
    },
    get dark() { return document.documentElement.getAttribute('data-theme') === 'dark' },
  })

  /* Links open in the browser instead of replacing the widget. */
  document.addEventListener('click', function (event) {
    var target = event.target
    var link = target && target.closest ? target.closest('a[href]') : null
    if (!link) return
    var href = link.getAttribute('href') || ''
    if (href.charAt(0) === '#') return
    event.preventDefault()
    if (/^https?:/i.test(link.href)) window.rainy.openLink(link.href).catch(function () {})
  })

  function applyTheme(theme) {
    var lines = []
    for (var name in theme.vars) {
      if (!/^[a-z0-9-]+$/.test(name)) continue
      lines.push('--' + name + ':' + String(theme.vars[name]).replace(/[;{}<]/g, '') + ';')
    }
    themeStyle.textContent = ':root{' + lines.join('') + 'color-scheme:' + (theme.dark ? 'dark' : 'light') + '}'
    document.documentElement.setAttribute('data-theme', theme.dark ? 'dark' : 'light')
    window.dispatchEvent(new CustomEvent('rainythemechange', { detail: { dark: theme.dark } }))
  }

  function morph(html) {
    if (typeof Idiomorph !== 'undefined' && Idiomorph.morph) {
      try {
        Idiomorph.morph(document.body, html, { morphStyle: 'innerHTML' })
        return
      } catch (error) { /* fall through */ }
    }
    document.body.innerHTML = html
  }

  function loadScript(src, module) {
    return new Promise(function (resolve) {
      var script = document.createElement('script')
      if (module) script.type = 'module'
      script.src = src
      script.onload = function () { resolve() }
      script.onerror = function () { report('Could not load ' + src); resolve() }
      document.head.appendChild(script)
    })
  }

  /* Run one script element the markup contained; innerHTML never runs them. External ones finish loading first. */
  function rerun(old) {
    return new Promise(function (resolve) {
      var script = document.createElement('script')
      for (var index = 0; index < old.attributes.length; index++) script.setAttribute(old.attributes[index].name, old.attributes[index].value)
      if (old.src && script.type !== 'module') {
        script.onload = function () { resolve() }
        script.onerror = function () { report('Could not load ' + old.getAttribute('src')); resolve() }
      }
      script.textContent = old.textContent
      old.parentNode.replaceChild(script, old)
      if (!old.src || script.type === 'module') resolve()
    })
  }

  function runInline(code) {
    var script = document.createElement('script')
    script.textContent = code
    document.body.appendChild(script)
  }

  function finish(message) {
    finished = true
    answerStyle.textContent = message.css
    morph(message.html)
    var code = message.script
    var inline = Array.prototype.slice.call(document.body.querySelectorAll('script'))
    var mentioned = code + inline.map(function (script) { return script.textContent }).join('\n')
    var chain = Promise.resolve()
    GLOBALS.forEach(function (library) {
      if (library.pattern.test(mentioned) && typeof window[library.name] === 'undefined') {
        chain = chain.then(function () { return loadScript(LIB + library.file, false) })
      }
    })
    inline.forEach(function (script) { chain = chain.then(function () { return rerun(script) }) })
    chain.then(function () {
      if (code.trim() !== '') runInline(code)
      measure()
      document.dispatchEvent(new Event('rainyready'))
    })
  }

  window.addEventListener('message', function (event) {
    if (event.source !== host) return
    var data = event.data
    if (!data || data.tag !== TAG) return
    if (data.type === 'theme') applyTheme(data.theme)
    else if (data.type === 'preview' && !finished) {
      answerStyle.textContent = data.css
      morph(data.html)
    } else if (data.type === 'final' && !finished) finish(data)
    else if (data.type === 'reply') {
      var pending = calls[data.id]
      if (!pending) return
      delete calls[data.id]
      if (data.ok) pending.resolve()
      else pending.reject(new Error(data.error || 'refused'))
    }
  })

  var last = 0
  function measure() {
    var height = Math.ceil(document.documentElement.getBoundingClientRect().height)
    if (height === last) return
    last = height
    post({ type: 'resize', height: height })
  }
  new ResizeObserver(measure).observe(document.documentElement)
  window.addEventListener('load', measure)

  post({ type: 'ready' })
})()
