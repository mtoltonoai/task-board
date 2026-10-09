import test from 'node:test'
import assert from 'node:assert/strict'
import { resolveActor } from '../src/actorSelection.ts'

test('generic fallback and stored human are not silently aliased', () => {
  assert.equal(resolveActor(null, null), 'human')
  assert.equal(resolveActor(null, 'human'), 'human')
})

test('stored actor remains the selected viewer without a replacement', () => {
  assert.equal(resolveActor(null, 'alice'), 'alice')
  assert.equal(resolveActor(null, 'matthew'), 'matthew')
})

test('forced identity wins over stored selection', () => {
  assert.equal(resolveActor('alice', 'matthew'), 'alice')
  assert.equal(resolveActor('matthew', 'human'), 'matthew')
})
