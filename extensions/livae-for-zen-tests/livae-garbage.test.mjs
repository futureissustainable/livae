import test from 'node:test'
import assert from 'node:assert/strict'
import { load } from './fake.mjs'

test('Livae answering garbage keeps the defaults', async () => {
  const { fake } = await load({ sites: [{ id: 1 }], quotes: [] })
  assert.equal(fake.rulesNow().length, 4)
})
