// Viewing and answering use the same actor. Never infer an identity from the people list.
export function resolveActor(forcedUser: string | null, storedActor: string | null): string {
  return forcedUser ?? storedActor ?? 'human'
}
