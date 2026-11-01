import test from 'node:test'
import assert from 'node:assert/strict'
import { load } from './fake.mjs'

test('without Livae: built-in sites, one rule per site', async () => {
  const { fake, ask } = await load(new Error('No such native application'))
  assert.equal(fake.rulesNow().length, 4)
  const info = await ask({ type: 'gate:info', url: 'https://www.reddit.com/' })
  assert.equal(info.seconds, 20)
})
