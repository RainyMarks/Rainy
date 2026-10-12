/**
 * Interface language of a setup window: main opens the page with `?lang=en` for English. Static text is written in
 * Chinese and carries its English version in `data-en` (text), `data-en-label` (accessible name) and, on `<body>`,
 * `data-en-title` (window title). Page scripts choose their own messages with `say(zh, en)`.
 */
const english = new URLSearchParams(location.search).get('lang') === 'en'
window.say = (zh, en) => english ? en : zh
window.setupLocale = english ? 'en-US' : 'zh-CN'
if (english) {
  document.documentElement.lang = 'en'
  for (const element of document.querySelectorAll('[data-en]')) element.textContent = element.dataset.en
  for (const element of document.querySelectorAll('[data-en-label]')) element.setAttribute('aria-label', element.dataset.enLabel)
  if (document.body.dataset.enTitle) document.title = document.body.dataset.enTitle
}
