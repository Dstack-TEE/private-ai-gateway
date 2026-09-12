/** Bounded, in-memory snapshots for immediate rendering while runtime data refreshes. */
export class ViewCache<T> {
  private readonly entries = new Map<string, T>();

  get(key: string): T | undefined { return this.entries.get(key); }

  set(key: string, value: T): void {
    this.entries.delete(key);
    this.entries.set(key, value);
    if (this.entries.size > 32) {
      const oldest = this.entries.keys().next();
      if (!oldest.done) this.entries.delete(oldest.value);
    }
  }
}
