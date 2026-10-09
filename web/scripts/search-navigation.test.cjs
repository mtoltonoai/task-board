// Actual component regression, adapted from the independent review_14 reproduction.
const fs = require('node:fs')
const path = require('node:path')
const vm = require('node:vm')
const assert = require('node:assert/strict')
const test = require('node:test')
const ts = require('typescript')
const root = path.join(__dirname, '../src')
const settle = () => new Promise(resolve => setImmediate(resolve))

function harness() {
  const hooks = []
  let cursor = 0, effects = [], params = new URLSearchParams('status=todo&assignee=')
  let resolveMutation
  const mutation = new Promise(resolve => { resolveMutation = resolve })
  const queries = []
  const react = {
    useState(initial) {
      const i = cursor++
      if (!(i in hooks)) hooks[i] = typeof initial === 'function' ? initial() : initial
      return [hooks[i], value => { hooks[i] = value }]
    },
    useRef(initial) { const i = cursor++; return hooks[i] ??= { current: initial } },
    useEffect(fn, deps) {
      const i = cursor++, old = hooks[i]
      if (!old || deps.some((d, n) => d !== old.deps[n])) effects.push(() => {
        old?.cleanup?.()
        hooks[i] = { deps, cleanup: fn() }
      })
    },
  }
  const jsx = (type, props) => ({ type, props })
  function load(file) {
    const code = ts.transpileModule(fs.readFileSync(path.join(root, file), 'utf8'), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    }).outputText
    const module = { exports: {} }
    vm.runInNewContext(code, { exports: module.exports, require: stub, URLSearchParams, Map,
      window: { alert: message => assert.fail(message), prompt: () => 'note' } })
    return module.exports
  }
  const helper = load('taskSearch.ts')
  function stub(name) {
    if (name === 'react') return react
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'fragment' }
    if (name === 'react-router-dom') return { Link: 'a', useSearchParams: () => [params, () => {}] }
    if (name === './scrollRestore') return { useScrollRestoration: () => null }
    if (name === './Layout') return { useBoardContext: () => ({ actor: 'alice' }) }
    if (name === './resources') return { useProjects: () => ({ data: [] }), updateTask: () => mutation, commentTask: () => mutation }
    if (name === './taskSearch') return helper
    if (name === './ui') return { Identity: 'identity', StatusChip: 'chip', STATUS_LABEL: {}, TASK_COLUMNS: ['todo', 'done'], relTime: () => '' }
    if (name === './api') return { api: { listTasks: async q => {
      queries.push(q.status)
      return [{ id: q.status === 'todo' ? 1 : 2, title: q.status, status: q.status, project_id: 1 }]
    } } }
    throw Error(name)
  }
  const Search = load('Search.tsx').default
  function render() { cursor = 0; effects = []; const tree = Search(); effects.forEach(fn => fn()); return tree }
  return { render, queries, resolveMutation,
    navigate() { params = new URLSearchParams('status=blocked&assignee='); render() },
    unmount() { for (const hook of hooks) hook?.cleanup?.() },
  }
}
function find(node, predicate) {
  if (!node) return
  if (Array.isArray(node)) { for (const child of node) { const found = find(child, predicate); if (found) return found } }
  else if (typeof node === 'object') {
    if (predicate(node)) return node
    return find(node.props?.children, predicate)
  }
}
for (const kind of ['status', 'comment']) {
  for (const unmount of [false, true]) {
    test(`delayed ${kind} completion respects ${unmount ? 'unmount' : 'current search'}`, async () => {
      const h = harness()
      h.render(); await settle()
      const tree = h.render()
      if (kind === 'status') find(tree, n => n.props?.title === 'change status').props.onChange({ target: { value: 'done' } })
      else find(tree, n => n.type === 'button' && n.props?.children === 'comment').props.onClick()
      h.navigate(); await settle()
      assert.equal(find(h.render(), n => n.props?.title === 'change status').props.value, 'blocked')
      if (unmount) h.unmount()
      h.resolveMutation(); await settle(); await settle()
      assert.deepEqual(h.queries, unmount ? ['todo', 'blocked'] : ['todo', 'blocked', 'blocked'])
      if (!unmount) assert.equal(find(h.render(), n => n.props?.title === 'change status').props.value, 'blocked')
    })
  }
}
