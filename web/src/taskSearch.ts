export const SEARCH_STATUSES = ['todo', 'in_progress', 'blocked', 'done', 'cancelled', 'icebox'] as const
export type SearchStatus = '' | 'open' | 'active' | typeof SEARCH_STATUSES[number]
export interface TaskFilters { q: string; assignee: string; status: SearchStatus; project: string; archived: boolean }

export function readTaskFilters(params: URLSearchParams, actor: string): TaskFilters {
  const explicit = ['q', 'assignee', 'status', 'project_id', 'include_archived'].some(k => params.has(k))
  const status = params.get('status') ?? ''
  const project = params.get('project_id') ?? ''
  return {
    q: params.get('q') ?? '',
    assignee: params.get('assignee') ?? (explicit ? '' : actor),
    status: [...SEARCH_STATUSES, 'open', 'active'].includes(status) ? status as SearchStatus : '',
    project: /^[1-9]\d*$/.test(project) && Number.isSafeInteger(Number(project)) ? project : '',
    archived: params.get('include_archived') === 'true',
  }
}

export function taskSearchParams(f: TaskFilters): URLSearchParams {
  // An explicit empty assignee distinguishes everyone from the bare personal search.
  const p = new URLSearchParams({ assignee: f.assignee.trim() })
  if (f.q.trim()) p.set('q', f.q.trim())
  if (f.status) p.set('status', f.status)
  if (f.project) p.set('project_id', f.project)
  if (f.archived) p.set('include_archived', 'true')
  return p
}

export function dashboardTaskHref(status: SearchStatus, project?: number): string {
  return `/search?${taskSearchParams({ q: '', assignee: '', status, project: project == null ? '' : String(project), archived: true })}`
}

export function taskSearchQueries(f: TaskFilters) {
  const statuses = f.status === 'open' ? ['todo', 'in_progress', 'blocked', 'icebox']
    : f.status === 'active' ? ['todo', 'in_progress', 'blocked'] : [f.status]
  return statuses.map(status => ({
    q: f.q.trim() || undefined,
    assignee: f.assignee.trim() || undefined,
    status: status || undefined,
    project_id: f.project ? Number(f.project) : undefined,
    include_archived: f.archived,
  }))
}
