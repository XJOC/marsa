import { reportError } from "./api";

// Keeps the most recent batches of rows that came from Rust. A batch is
// `batchSize` consecutive rows, fetched with one `invoke`. The cache is a
// small LRU (a `Map` keeps insertion order), so scrolling through 100,000
// rows never keeps them all in the webview.

export class BatchCache<T> {
  private readonly batches = new Map<number, T[]>();
  private readonly inFlight = new Set<number>();

  constructor(
    private readonly fetchBatch: (start: number, count: number) => Promise<T[]>,
    private readonly total: number,
    private readonly batchSize: number,
    private readonly capacity: number,
    private readonly onLoad: () => void,
  ) {}

  /** The row at `index`, or `undefined` while its batch is not loaded. */
  get(index: number): T | undefined {
    const batch = Math.floor(index / this.batchSize);
    return this.batches.get(batch)?.[index - batch * this.batchSize];
  }

  /**
   * Loads the batches that cover rows `first..=last`, plus one batch on each
   * side, so a scroll usually finds the next rows already loaded.
   */
  ensure(first: number, last: number): void {
    if (this.total === 0) return;
    const lastBatch = Math.floor((this.total - 1) / this.batchSize);
    const from = Math.max(0, Math.floor(first / this.batchSize) - 1);
    const to = Math.min(lastBatch, Math.floor(last / this.batchSize) + 1);
    for (let batch = from; batch <= to; batch++) {
      const rows = this.batches.get(batch);
      if (rows !== undefined) {
        // Mark as recently used.
        this.batches.delete(batch);
        this.batches.set(batch, rows);
        continue;
      }
      if (this.inFlight.has(batch)) continue;
      this.inFlight.add(batch);
      this.fetchBatch(batch * this.batchSize, this.batchSize).then(
        (loaded) => {
          this.inFlight.delete(batch);
          this.batches.set(batch, loaded);
          while (this.batches.size > this.capacity) {
            const oldest = this.batches.keys().next();
            if (oldest.done === true) break;
            this.batches.delete(oldest.value);
          }
          this.onLoad();
        },
        (error: unknown) => {
          this.inFlight.delete(batch);
          reportError(`batch ${batch}`, error);
        },
      );
    }
  }
}
