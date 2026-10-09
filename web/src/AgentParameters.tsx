import { Fragment } from 'react'
import { agentParameters } from './agentParameters'

export default function AgentParameters({ agent, compact = false }: {
  agent: { metadata?: unknown; lifecycle_intent?: unknown }
  compact?: boolean
}) {
  const rows = agentParameters(agent)
  return (
    <section aria-label="Agent configuration" className={compact ? 'mt-2 pl-4' : 'mb-5'}>
      <dl className="grid grid-cols-2 gap-x-3 gap-y-1 text-xs">
        {rows.map(([label, value]) => (
          <Fragment key={label}>
            <dt className="text-[var(--color-muted)]">{label}</dt>
            <dd className="min-w-0 break-words font-mono">{value}</dd>
          </Fragment>
        ))}
      </dl>
      <p className="mt-2 text-[11px] text-[var(--color-muted)]">
        Active-turn settings: unknown. Registry configuration does not verify runtime settings.
      </p>
    </section>
  )
}
