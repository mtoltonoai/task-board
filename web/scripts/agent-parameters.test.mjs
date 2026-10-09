import test from 'node:test'
import assert from 'node:assert/strict'
import { agentParameters, configuredValue } from '../src/agentParameters.ts'

test('shows explicit configuration and lifecycle without invented defaults', () => {
  assert.deepEqual(agentParameters({ metadata: { model: 'model-a', effort: 'max', harness: 'codex', host: 'agent-lab', interval: '120s' }, lifecycle_intent: 'run' }), [
    ['Configured model', 'model-a'], ['Configured reasoning effort', 'max'], ['Configured harness', 'codex'],
    ['Configured host', 'agent-lab'], ['Configured cadence', '120s'], ['Desired lifecycle', 'run'],
  ])
})
test('missing and malformed metadata values are unknown', () => {
  for (const metadata of [undefined, null, [], 'raw', { model: {}, effort: false, interval: 120 }]) {
    assert.ok(agentParameters({ metadata }).every(([, value]) => value === 'unknown'))
  }
  for (const value of ['', '  ', 'a\nb', 'x'.repeat(161), null, 0]) assert.equal(configuredValue(value), 'unknown')
})
test('does not surface unlisted secrets, arguments, or claimed runtime evidence', () => {
  const metadata = { env: { TOKEN: 'secret' }, argv: ['--secret'], credentials: 'secret', effective_model: 'claimed-model', runtime_verified: true }
  assert.ok(agentParameters({ metadata }).every(([, value]) => value === 'unknown'))
  assert.ok(!JSON.stringify(agentParameters({ metadata })).includes('secret'))
})

test('rendered configuration separates unknown runtime and escapes text in both views', async () => {
  const { readFileSync } = await import('node:fs')
  const { createRequire } = await import('node:module')
  const { runInNewContext } = await import('node:vm')
  const require = createRequire(import.meta.url)
  const ts = require('typescript')
  const React = require('react')
  const { renderToStaticMarkup } = require('react-dom/server')
  const source = readFileSync(new URL('../src/AgentParameters.tsx', import.meta.url), 'utf8')
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } }).outputText
  const module = { exports: {} }
  runInNewContext(code, { exports: module.exports, require: name => name === './agentParameters' ? { agentParameters } : require(name) })
  for (const compact of [false, true]) {
    const html = renderToStaticMarkup(React.createElement(module.exports.default, { compact, agent: { metadata: { model: '<model>', token: 'secret', effective_model: 'unverified' } } }))
    assert.match(html, /Configured model/)
    assert.match(html, /&lt;model&gt;/)
    assert.match(html, /Active-turn settings: unknown/)
    assert.match(html, /Registry configuration does not verify runtime settings/)
    assert.ok(!html.includes('secret') && !html.includes('unverified'))
  }
})
