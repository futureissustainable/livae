// Livae for Zen reads its sites, waits and quotes from Livae when Livae is installed (native messaging host
// com.mateus.livae). Without Livae, or when it does not answer, the built-in config.js and quotes.json stay in use.

import { SITES, PASS_MINUTES } from './config.js'

const api = globalThis.browser ?? globalThis.chrome
const HOST = 'com.mateus.livae'

const validSite = s => s && typeof s.id === 'string' && typeof s.label === 'string' &&
  Array.isArray(s.domains) && s.domains.length > 0 && s.domains.every(d => typeof d === 'string') &&
  Number.isFinite(s.seconds) && Number.isFinite(s.step)

const validQuote = q => q && typeof q.text === 'string' && q.text.trim() !== ''

// Ask Livae once; keep what it says in storage so the gate works the same while Livae is not running.
export async function refreshFromLivae () {
  try {
    const reply = await api.runtime.sendNativeMessage(HOST, { get: 'gate' })
    const sites = (reply?.sites || []).filter(validSite)
    const quotes = (reply?.quotes || []).filter(validQuote)
    if (!sites.length && !quotes.length) return false
    await api.storage.local.set({
      livae: {
        sites: sites.length ? sites : null,
        passMinutes: Number.isFinite(reply.pass_minutes) && reply.pass_minutes > 0 ? reply.pass_minutes : null,
        quotes: quotes.length ? quotes.map(q => ({ text: q.text, author: q.author || '' })) : null,
        at: Date.now()
      }
    })
    return true
  } catch {
    return false // Livae not installed or not answering: keep the last copy or the built-in defaults
  }
}

export async function currentSites () {
  const { livae } = await api.storage.local.get('livae')
  return livae?.sites || SITES
}

export async function passMinutes () {
  const { livae } = await api.storage.local.get('livae')
  return livae?.passMinutes || PASS_MINUTES
}

export async function currentQuotes () {
  const { livae } = await api.storage.local.get('livae')
  if (livae?.quotes?.length) return livae.quotes
  return (await fetch(api.runtime.getURL('quotes.json'))).json()
}
