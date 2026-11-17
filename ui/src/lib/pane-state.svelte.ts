// Presentation of transport state; Rust remains the source of project state.
export const paneConnection = $state({
  online: true,
  rosenReconnecting: false,
});
export function paneFailure(
  name: string,
  error: string,
  automatic = true,
): string {
  if (/gh auth|authentication|not logged|sign.?in|401|403/i.test(error))
    return /Issues|Changes/.test(name)
      ? `${name} unavailable · Sign in to GitHub with gh auth login, then retry.`
      : `${name} unavailable · Sign in through Settings → Providers.`;
  if (!automatic) return `${name} failed · Retry.`;
  if (
    /rosen/i.test(error) ||
    (/Machines|Workspaces|AIRC/.test(name) &&
      /connection refused|server closed/i.test(error))
  )
    return `${name} unavailable · Rosen is reconnecting.`;
  if (/network|offline|dns|unreachable|timed? ?out|connect/i.test(error))
    return `${name} offline · Reconnecting automatically.`;
  return `${name} could not load · Retry.`;
}
