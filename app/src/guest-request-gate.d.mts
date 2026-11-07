
export interface RequestGate<T> {
  get(vmId: string): Promise<T> | undefined;
  has(vmId: string): boolean;
  start(vmId: string, factory: () => Promise<T>): Promise<T>;
}
export function createRequestGate<T>(): RequestGate<T>;