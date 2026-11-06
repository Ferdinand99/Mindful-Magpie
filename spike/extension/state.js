// Pure view logic for the new-tab page. No browser APIs here, so it is testable with node --test.
export function nextView(cached, fetchResult) {
  if (fetchResult === undefined) {
    return { data: cached ?? null, status: cached ? "cached" : "empty" };
  }
  if (fetchResult.ok) {
    return { data: fetchResult.data, status: "fresh" };
  }
  if (!cached) {
    return { data: null, status: "empty" };
  }
  if (fetchResult.kind === "unauthorized") {
    return { data: cached, status: "unauthorized" };
  }
  return { data: cached, status: "offline" };
}
