// Runs Livae for Zen's background script against a fake browser API: with Livae answering, and without it.

export function fakeBrowser (nativeReply) {
  const store = {}
  let rules = []
  const listeners = { installed: [], startup: [], alarm: [], message: [] }
  return {
    rulesNow: () => rules,
    listeners,
    api: {
      storage: { local: { get: async k => ({ [k]: store[k] }), set: async o => Object.assign(store, o) } },
      declarativeNetRequest: {
        getDynamicRules: async () => rules,
        updateDynamicRules: async ({ removeRuleIds, addRules }) => {
          rules = rules.filter(r => !removeRuleIds.includes(r.id)).concat(addRules)
        }
      },
      alarms: { create: () => {}, onAlarm: { addListener: f => listeners.alarm.push(f) } },
      runtime: {
        getURL: p => 'moz-extension://x/' + p,
        sendNativeMessage: async () => { if (nativeReply instanceof Error) throw nativeReply; return nativeReply },
        onInstalled: { addListener: f => listeners.installed.push(f) },
        onStartup: { addListener: f => listeners.startup.push(f) },
        onMessage: { addListener: f => listeners.message.push(f) }
      },
      tabs: { remove: () => {} }
    }
  }
}

export async function load (nativeReply) {
  const fake = fakeBrowser(nativeReply)
  globalThis.browser = fake.api
  await import('../livae-for-zen/background.js')
  await fake.listeners.installed[0]()
  const ask = message => new Promise(resolve => fake.listeners.message[0](message, {}, resolve))
  return { fake, ask }
}

