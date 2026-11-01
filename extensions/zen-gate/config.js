// Zen Gate — the only file you need to edit to tune behaviour.

// Each entry is one gate. `domains` covers the domain and all its subdomains
// (www.reddit.com, old.reddit.com, m.youtube.com all match).
//
// `seconds` is the first wait of the day. `step` is what each pass adds to it
// for the rest of the day — 0 means the wait never grows. Resets at local
// midnight, and there is no ceiling where step > 0.
export const SITES = [
  { id: 'x', label: 'X', domains: ['twitter.com', 'x.com'], seconds: 20, step: 5 },
  { id: 'reddit', label: 'Reddit', domains: ['reddit.com'], seconds: 20, step: 5 },
  { id: 'youtube', label: 'YouTube', domains: ['youtube.com'], seconds: 5, step: 0 },
  { id: 'instagram', label: 'Instagram', domains: ['instagram.com'], seconds: 5, step: 0 }
]

// Once through, how long the site stays open before the gate returns.
export const PASS_MINUTES = 15

export function siteForUrl (url, sites = SITES) {
  let host
  try {
    const parsed = new URL(url)
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return null
    host = parsed.hostname.toLowerCase()
  } catch {
    return null
  }
  return sites.find(site =>
    site.domains.some(domain => host === domain || host.endsWith('.' + domain))
  ) || null
}
