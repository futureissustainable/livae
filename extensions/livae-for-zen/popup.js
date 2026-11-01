// Livae for Zen — what today has cost you, per site.

const api = globalThis.browser ?? globalThis.chrome

const list = document.querySelector('.sites')
const sites = await api.runtime.sendMessage({ type: 'gate:status' })

for (const site of sites) {
  const li = document.createElement('li')

  const name = document.createElement('span')
  name.textContent = site.label

  const wait = document.createElement('span')
  wait.className = 'wait'

  if (site.openFor !== null) {
    li.className = 'open'
    const mins = Math.max(1, Math.round(site.openFor / 60_000))
    wait.append('open / ', Object.assign(document.createElement('b'), {
      textContent: mins + 'm'
    }))
  } else {
    if (site.opens) wait.append(site.opens + ' today / ')
    wait.append(Object.assign(document.createElement('b'), {
      textContent: site.seconds + 's'
    }))
  }

  li.append(name, wait)
  list.append(li)
}
