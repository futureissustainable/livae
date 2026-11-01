import { siteForUrl } from './config.js'
import { refreshFromLivae, currentSites, passMinutes, currentQuotes } from './livae.js'

// Firefox exposes the promise-based `browser`; Chrome only `chrome`.
const api = globalThis.browser ?? globalThis.chrome

const PASS_ALARM = 'pass:'
const LIVAE_ALARM = 'livae:refresh'

const today = () => new Date().toLocaleDateString('en-CA')

// ---------- state ----------
// { day: '2026-08-01', counts: { youtube: 3 }, passes: { youtube: 1754040000000 } }

async function readState () {
  const { state } = await api.storage.local.get('state')
  const fresh = { day: today(), counts: {}, passes: {}, ...(state || {}) }
  if (fresh.day !== today()) {
    fresh.day = today()
    fresh.counts = {}
  }
  return fresh
}

const writeState = state => api.storage.local.set({ state })

const passActive = (state, siteId) => (state.passes[siteId] || 0) > Date.now()

// ---------- blocking ----------

function ruleFor (site, id) {
  return {
    id,
    priority: 1,
    action: {
      type: 'redirect',
      // \0 is the whole matched URL. It is appended last and read back on the
      // gate page as "everything after the first ?u=", so no escaping is needed.
      redirect: { regexSubstitution: api.runtime.getURL('gate.html') + '?u=\\0' }
    },
    condition: {
      regexFilter: '^.*$',
      requestDomains: site.domains,
      resourceTypes: ['main_frame']
    }
  }
}

// Rules exist for every site that is not currently holding a pass. The site list can change (Livae), so the
// old rules are removed by what is installed, not by what the list says now.
async function syncRules (state) {
  const sites = await currentSites()
  const wanted = sites
    .map((site, i) => ({ site, id: i + 1 }))
    .filter(({ site }) => !passActive(state, site.id))
    .map(({ site, id }) => ruleFor(site, id))
  const installed = await api.declarativeNetRequest.getDynamicRules()
  await api.declarativeNetRequest.updateDynamicRules({
    removeRuleIds: installed.map(rule => rule.id),
    addRules: wanted
  })
}

async function reconcile () {
  const state = await readState()
  for (const siteId of Object.keys(state.passes)) {
    if (passActive(state, siteId)) {
      api.alarms.create(PASS_ALARM + siteId, { when: state.passes[siteId] })
    } else {
      delete state.passes[siteId]
    }
  }
  await writeState(state)
  await syncRules(state)
}

async function startup () {
  await refreshFromLivae()
  api.alarms.create(LIVAE_ALARM, { periodInMinutes: 60 })
  await reconcile()
}

api.runtime.onInstalled.addListener(startup)
api.runtime.onStartup.addListener(startup)

api.alarms.onAlarm.addListener(async alarm => {
  if (alarm.name === LIVAE_ALARM) {
    if (await refreshFromLivae()) await syncRules(await readState())
    return
  }
  if (!alarm.name.startsWith(PASS_ALARM)) return
  const siteId = alarm.name.slice(PASS_ALARM.length)
  const state = await readState()
  delete state.passes[siteId]
  await writeState(state)
  await syncRules(state)
})

// ---------- gate page API ----------

async function gateInfo (url) {
  const site = siteForUrl(url, await currentSites())
  if (!site) return null
  const state = await readState()
  const opens = state.counts[site.id] || 0
  return {
    siteId: site.id,
    label: site.label,
    visit: opens + 1,
    seconds: site.seconds + site.step * opens
  }
}

async function grantPass (siteId) {
  if (!(await currentSites()).some(site => site.id === siteId)) return { ok: false }
  const state = await readState()
  state.counts[siteId] = (state.counts[siteId] || 0) + 1
  state.passes[siteId] = Date.now() + (await passMinutes()) * 60_000
  await writeState(state)
  await syncRules(state)
  api.alarms.create(PASS_ALARM + siteId, { when: state.passes[siteId] })
  return { ok: true }
}

async function status () {
  const state = await readState()
  return (await currentSites()).map(site => {
    const opens = state.counts[site.id] || 0
    return {
      label: site.label,
      opens,
      seconds: site.seconds + site.step * opens,
      openFor: passActive(state, site.id) ? state.passes[site.id] - Date.now() : null
    }
  })
}

api.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message?.type === 'gate:status') {
    status().then(sendResponse)
    return true
  }
  if (message?.type === 'gate:quotes') {
    currentQuotes().then(sendResponse)
    return true
  }
  if (message?.type === 'gate:info') {
    gateInfo(message.url).then(sendResponse)
    return true
  }
  if (message?.type === 'gate:pass') {
    grantPass(message.siteId).then(sendResponse)
    return true
  }
  if (message?.type === 'gate:leave') {
    if (sender.tab?.id) api.tabs.remove(sender.tab.id)
    sendResponse({ ok: true })
    return true
  }
})
