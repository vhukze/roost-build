
/** Per-VM promise identity only; no timer, command, UI or automatic request. */
export function createRequestGate() {
  const pending = new Map();
  return {
    get: (vmId) => pending.get(vmId),
    has: (vmId) => pending.has(vmId),
    start(vmId, factory) {
      const existing = pending.get(vmId);
      if (existing) return existing;
      // A synchronous failure must leave no entry. The factory is user-invoked.
      const request = Promise.resolve(factory());
      pending.set(vmId, request);
      const release = () => { if (pending.get(vmId) === request) pending.delete(vmId); };
      void request.then(release, release);
      return request;
    },
  };
}