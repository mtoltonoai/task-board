import { useEffect, useRef, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { useScrollRestoration } from './scrollRestore'
import { api, type TaskStatus, type TaskSummary } from './api'
import { useBoardContext } from './Layout'
import { commentTask, updateTask, useProjects } from './resources'
import { readTaskFilters, taskSearchParams, taskSearchQueries, SEARCH_STATUSES, type TaskFilters, type SearchStatus } from './taskSearch'
import { Identity, STATUS_LABEL, StatusChip, TASK_COLUMNS, relTime } from './ui'

// Cross-project search + a personal "my tasks" view. Unlike the per-project Board, this queries
// the WHOLE board (no project_id) so you can find or triage tasks anywhere — filtered by free
// text, assignee (defaults to you), and status (e.g. blocked) — with inline status + comment
// actions. It fetches on demand (search is transient) rather than living in the resource store,
// and re-runs after each mutation so the list stays current.
export default function Search() {
  const scrollRef = useScrollRestoration()
  const { actor } = useBoardContext()
  const { data: projects = [] } = useProjects()
  const projectName = (id?: number) =>
    projects.find((p) => p.id === id)?.name ?? (id != null ? `#${id}` : '')

  // Filters are hydrated from the URL query so a filtered view is bookmarkable + shareable
  // (task 524). A bare /search (no params) defaults to your own tasks (the my-tasks view); once
  // ANY filter param is present the URL is authoritative — so `?status=blocked` with no assignee
  // means "anyone blocked", and `?assignee=<you>&status=blocked` is the bookmarkable "blocked on me".
  const [searchParams, setSearchParams] = useSearchParams()
  const initial = readTaskFilters(searchParams, actor)
  const [q, setQ] = useState(initial.q)
  const [assignee, setAssignee] = useState(initial.assignee)
  const [status, setStatus] = useState<SearchStatus>(initial.status)
  const [project, setProject] = useState(initial.project)
  const [archived, setArchived] = useState(initial.archived)
  const [results, setResults] = useState<TaskSummary[] | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const request = useRef(0)
  const refreshCommitted = useRef<(() => Promise<void>) | null>(null)

  function syncUrl(vals: { q: string; assignee: string; status: SearchStatus }) {
    const filters = { ...vals, project, archived }
    const next = taskSearchParams(filters)
    if (next.toString() === searchParams.toString()) void run(filters)
    else setSearchParams(next)
  }

  async function run(filters: TaskFilters = readTaskFilters(searchParams, actor)) {
    const generation = ++request.current
    setResults(null)
    setLoading(true)
    setError(null)
    try {
      const pages = await Promise.all(taskSearchQueries(filters).map(query => api.listTasks(query)))
      if (request.current === generation) setResults([...new Map(pages.flat().map(t => [t.id, t])).values()].sort((a, b) => a.id - b.id))
    } catch (e) {
      if (request.current === generation) setError((e as Error).message)
    } finally {
      if (request.current === generation) setLoading(false)
    }
  }

  // URL navigation (including Back/Forward) is the committed search; form inputs are drafts.
  useEffect(() => {
    const filters = readTaskFilters(searchParams, actor)
    setQ(filters.q)
    setAssignee(filters.assignee)
    setStatus(filters.status)
    setProject(filters.project)
    setArchived(filters.archived)
    refreshCommitted.current = () => run(filters)
    void run(filters)
    return () => { refreshCommitted.current = null; request.current++ }
  }, [searchParams, actor])

  async function setTaskStatus(id: number, s: TaskStatus) {
    try {
      await updateTask(id, { status: s, principal: actor })
      await refreshCommitted.current?.()
    } catch (e) {
      window.alert((e as Error).message)
    }
  }

  async function addComment(id: number) {
    const body = window.prompt('Comment:')
    if (!body?.trim()) return
    try {
      await commentTask(id, { body: body.trim(), principal: actor })
      await refreshCommitted.current?.()
    } catch (e) {
      window.alert((e as Error).message)
    }
  }

  return (
    <main className="flex min-w-0 flex-1 flex-col">
      <div className="border-b border-[var(--color-border)] px-5 py-3">
        <h1 className="text-sm font-semibold">Search &amp; my tasks</h1>
        <form
          className="mt-2 flex flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault()
            syncUrl({ q, assignee, status })
          }}
        >
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="search title or description…"
            className="w-64 rounded-md border border-[var(--color-border)] bg-[var(--color-panel-2)] px-2 py-1 text-sm outline-none focus:border-sky-500/50"
          />
          <input
            value={assignee}
            onChange={(e) => setAssignee(e.target.value)}
            placeholder="assignee (blank = anyone)"
            className="w-44 rounded-md border border-[var(--color-border)] bg-[var(--color-panel-2)] px-2 py-1 font-mono text-xs outline-none focus:border-sky-500/50"
          />
          <select
            value={status}
            onChange={(e) => {
              const s = e.target.value as SearchStatus
              setStatus(s)
              syncUrl({ q, assignee, status: s })
            }}
            className="rounded-md border border-[var(--color-border)] bg-[var(--color-panel-2)] px-2 py-1 text-sm outline-none focus:border-sky-500/50"
          >
            <option value="">any status</option>
            <option value="open">open (including icebox)</option>
            <option value="active">todo, in progress or blocked</option>
            {SEARCH_STATUSES.map((s) => (
              <option key={s} value={s}>
                {s === 'icebox' ? 'Icebox' : STATUS_LABEL[s]}
              </option>
            ))}
          </select>
          <select aria-label="Project" value={project} onChange={e => setProject(e.target.value)} className="rounded border border-[var(--color-border)] bg-[var(--color-panel-2)] px-2 py-1 text-sm">
            <option value="">All projects</option>
            {projects.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
          </select>
          <label className="text-xs"><input type="checkbox" checked={archived} onChange={e => setArchived(e.target.checked)} /> Include archived tasks</label>
          <button
            type="submit"
            className="rounded-md bg-sky-500/20 px-3 py-1 text-sm text-sky-200 hover:bg-sky-500/30"
          >
            Search
          </button>
          <Link
            to="/awaiting"
            title="Everything awaiting your decision: blocked-on-you tasks + questions routed to you (team-expanded, not just tasks assigned to you)"
            className="rounded-md px-2 py-1 text-xs text-[var(--color-muted)] hover:bg-[var(--color-panel-2)]"
          >
            awaiting you
          </Link>
        </form>
      </div>

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-5 py-3">
        {error && <p className="text-sm text-rose-700 dark:text-rose-300">{error}</p>}
        {loading && <p className="text-sm text-[var(--color-muted)]">Searching…</p>}
        {!loading && results && results.length === 0 && (
          <p className="text-sm text-[var(--color-muted)]">No matching tasks.</p>
        )}
        <ul className="space-y-1.5">
          {results?.map((t) => (
            <li
              key={t.id}
              className="flex items-center gap-3 rounded-md border border-[var(--color-border)] bg-[var(--color-panel)] px-3 py-2"
            >
              <StatusChip status={t.status} />
              <Link
                to={`/projects/${t.project_id}/tasks/${t.id}`}
                className="min-w-0 flex-1 truncate text-sm hover:text-sky-800 dark:hover:text-sky-300"
              >
                {t.title}
              </Link>
              <span className="hidden text-xs text-[var(--color-muted)] sm:inline">
                {projectName(t.project_id)}
              </span>
              {t.assignee && (
                <Identity
                  id={t.assignee}
                  className="font-mono text-[11px] text-[var(--color-muted)]"
                />
              )}
              <span className="text-[11px] text-[var(--color-muted)]">{relTime(t.updated_at)}</span>
              <select
                value={t.status}
                onChange={(e) => void setTaskStatus(t.id, e.target.value as TaskStatus)}
                title="change status"
                className="rounded border border-[var(--color-border)] bg-[var(--color-panel-2)] px-1 py-0.5 text-xs outline-none"
              >
                {TASK_COLUMNS.map((s) => (
                  <option key={s} value={s}>
                    {STATUS_LABEL[s]}
                  </option>
                ))}
              </select>
              <button
                onClick={() => void addComment(t.id)}
                className="rounded px-1.5 py-0.5 text-xs text-sky-700 dark:text-sky-400 hover:bg-[var(--color-panel-2)]"
              >
                comment
              </button>
            </li>
          ))}
        </ul>
      </div>
    </main>
  )
}
