import test from 'node:test'
import assert from 'node:assert/strict'
import { load } from './fake.mjs'

test('with Livae: its sites, waits and pass length win', async () => {
  const reply = {
    pass_minutes: 30,
    sites: [{ id: 'news', label: 'News', domains: ['news.example'], seconds: 7, step: 2 }],
    quotes: [{ text: 'Life is priceless.', author: 'Introspection' }]
  }
  const { fake, ask } = await load(reply)
  assert.deepEqual(fake.rulesNow().map(r => r.condition.requestDomains), [['news.example']])
  const info = await ask({ type: 'gate:info', url: 'https://news.example/a' })
  assert.equal(info.seconds, 7)
  assert.equal(await ask({ type: 'gate:info', url: 'https://www.reddit.com/' }), null)
  const quotes = await ask({ type: 'gate:quotes' })
  assert.equal(quotes[0].author, 'Introspection')
  await ask({ type: 'gate:pass', siteId: 'news' })
  assert.equal(fake.rulesNow().length, 0, 'a pass lifts the rule')
  const again = await ask({ type: 'gate:status' })
  assert.equal(again[0].seconds, 9, 'each pass adds the step')
})
