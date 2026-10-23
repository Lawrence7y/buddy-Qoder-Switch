/** Keep unconfirmed activity receipts separate from both rewards and failures. */
export function summarizeCheckinResults(entries: { id: string; result: string }[]) {
  let ok = 0;
  let already = 0;
  const pendingIds: string[] = [];
  const failedIds: string[] = [];
  for (const entry of entries) {
    if (entry.result === "success") ok += 1;
    else if (entry.result === "already") already += 1;
    else if (entry.result === "pending") pendingIds.push(entry.id);
    else failedIds.push(entry.id);
  }
  return { ok, already, pendingIds, failedIds };
}
