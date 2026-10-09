import test from 'node:test'
import assert from 'node:assert/strict'
import { dashboardTaskHref, readTaskFilters, taskSearchParams, taskSearchQueries, SEARCH_STATUSES } from '../src/taskSearch.ts'

function fromLink(status, project) {
  return readTaskFilters(new URL(dashboardTaskHref(status, project), 'http://board/').searchParams, 'alice')
}
const tasks = [1, 2].flatMap(project_id => SEARCH_STATUSES.flatMap(status =>
  [false, true].map(archived => ({ project_id, status, archived, exempt: true, blocker: 'operator' }))))
function matching(filters) {
  return taskSearchQueries(filters).flatMap(query => tasks.filter(t =>
    (!query.status || t.status === query.status) &&
    (!query.project_id || t.project_id === query.project_id) &&
    (query.include_archived || !t.archived)))
}

test('every status link matches counted tasks including archived, exempt and blocked rows', () => {
  for (const status of SEARCH_STATUSES) {
    const filters = fromLink(status)
    assert.equal(filters.assignee, '')
    assert.deepEqual(matching(filters), tasks.filter(t => t.status === status))
  }
})

test('fleet open includes icebox; project open preserves three-status predicate and scope', () => {
  const ids = rows => rows.map(t => `${t.project_id}/${t.status}/${t.archived}`).sort()
  assert.deepEqual(ids(matching(fromLink('open'))), ids(tasks.filter(t => !['done', 'cancelled'].includes(t.status))))
  assert.deepEqual(ids(matching(fromLink('active', 2))), ids(tasks.filter(t => t.project_id === 2 && ['todo', 'in_progress', 'blocked'].includes(t.status))))
  assert.equal(matching(fromLink('blocked', 2)).length, 2)
})

test('encoded query roundtrip restores each history entry without personal defaults', () => {
  const f = { q: 'a & b/雪', assignee: 'a+b@example.com', status: 'icebox', project: '2', archived: true }
  const history = [new URLSearchParams(), taskSearchParams(f), taskSearchParams({ ...f, assignee: '', status: 'open' })]
  assert.equal(readTaskFilters(history[0], 'alice').assignee, 'alice')
  assert.deepEqual(readTaskFilters(history[1], 'alice'), f)
  assert.equal(readTaskFilters(history[2], 'alice').assignee, '')
  assert.deepEqual(readTaskFilters(history[1], 'alice'), f)
})

test('invalid project and status do not become API query values', () => {
  const f = readTaskFilters(new URLSearchParams('project_id=NaN&status=nonsense'), 'alice')
  assert.equal(f.project, '')
  assert.equal(f.status, '')
  assert.equal(taskSearchQueries(f)[0].project_id, undefined)
})
