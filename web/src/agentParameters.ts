// Display only named configuration fields. Arbitrary metadata is not runtime evidence.
export function configuredValue(value: unknown): string {
  if (typeof value !== 'string') return 'unknown'
  const text = value.trim()
  return text && text.length <= 160 && !/[\x00-\x1f\x7f]/.test(text) ? text : 'unknown'
}

export function agentParameters(agent: { metadata?: unknown; lifecycle_intent?: unknown }) {
  const raw = agent.metadata
  const metadata = raw && typeof raw === 'object' && !Array.isArray(raw) ? raw as Record<string, unknown> : {}
  return [
    ['Configured model', configuredValue(metadata.model)],
    ['Configured reasoning effort', configuredValue(metadata.effort)],
    ['Configured harness', configuredValue(metadata.harness)],
    ['Configured host', configuredValue(metadata.host)],
    ['Configured cadence', configuredValue(metadata.interval)],
    ['Desired lifecycle', configuredValue(agent.lifecycle_intent)],
  ] as const
}
