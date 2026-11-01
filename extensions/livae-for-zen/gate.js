// Livae for Zen — the pause itself.

// Firefox exposes the promise-based `browser`; Chrome only `chrome`.
const api = globalThis.browser ?? globalThis.chrome

const DISSOLVE = 240      // ms of fade-out before we hand over

const quoteEl = document.querySelector('.zen-quote')
const authorEl = document.querySelector('.zen-author')
const ideaEl = document.querySelector('.idea')
const timerEl = document.querySelector('.timer')
const metaEl = document.querySelector('.meta')
const actionsEl = document.querySelector('.actions')
const leaveEl = document.querySelector('#leave')

const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches

// The blocked URL is appended last as `?u=<raw url>`, unescaped, so everything
// after the first `?u=` is exactly the original address.
function target () {
  const i = location.href.indexOf('?u=')
  if (i === -1) return null
  const url = location.href.slice(i + 3)
  return /^https?:\/\//i.test(url) ? url : null
}

const pad = (n, l) => String(n).padStart(l, '0')

function format (ms) {
  if (ms < 0) ms = 0
  const totalSec = Math.ceil(ms / 1000)
  const mins = Math.floor(totalSec / 60)
  const secs = totalSec % 60
  return mins > 0 ? mins + ':' + pad(secs, 2) : secs + 's'
}

// ---- the quote -------------------------------------------------------------

async function showQuote () {
  // From Livae when it is installed (quotes.md), else the built-in list.
  const quotes = await api.runtime.sendMessage({ type: 'gate:quotes' })
  const last = Number(localStorage.getItem('zen:last'))
  let i = Math.floor(Math.random() * quotes.length)
  if (i === last) i = (i + 1) % quotes.length
  localStorage.setItem('zen:last', String(i))

  const { text, author } = quotes[i]
  const quote = text.replace(/[^\S\r\n]+/g, ' ').trim()
  authorEl.textContent = author

  quoteEl.textContent = quote
  ideaEl.classList.add('zen-typed')
}

// ---- the wait --------------------------------------------------------------

function countdown (seconds, onEnd) {
  const started = Date.now()
  const duration = seconds * 1000
  const tick = () => {
    const left = duration - (Date.now() - started)
    timerEl.textContent = format(left)
    if (left > 0) {
      requestAnimationFrame(tick)
    } else {
      timerEl.textContent = format(0)
      onEnd()
    }
  }
  requestAnimationFrame(tick)
}

// ---- handing over ----------------------------------------------------------

let handedOver = false

function dissolve (then) {
  document.body.classList.add('zen-out')
  setTimeout(then, reducedMotion ? 0 : DISSOLVE)
}

async function proceed (siteId, url) {
  if (handedOver) return
  handedOver = true
  await api.runtime.sendMessage({ type: 'gate:pass', siteId })
  dissolve(() => location.replace(url))
}

function leave () {
  if (handedOver) return
  handedOver = true
  dissolve(() => {
    if (history.length > 1) history.back()
    else api.runtime.sendMessage({ type: 'gate:leave' })
  })
}

// ---- start -----------------------------------------------------------------

async function start () {
  showQuote()

  const url = target()
  const info = url ? await api.runtime.sendMessage({ type: 'gate:info', url }) : null

  leaveEl.onclick = leave
  leaveEl.tabIndex = 0
  actionsEl.classList.add('ready')
  actionsEl.removeAttribute('aria-hidden')
  addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); leave() }
  })

  // Opened directly, or a site that is no longer gated: just the quote.
  if (!info) {
    timerEl.remove()
    leaveEl.textContent = 'Close'
    return
  }

  metaEl.textContent = info.label
  timerEl.textContent = format(info.seconds * 1000)
  countdown(info.seconds, () => proceed(info.siteId, url))
}

start()
