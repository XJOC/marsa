// A virtual list whose rows live in Rust. `@tanstack/react-virtual` decides
// which rows are visible (retained DOM: only those rows exist as elements);
// `BatchCache` fetches them from Rust in batches.
import { useEffect, useRef, useState } from "react";
import { useVirtualizer, type Virtualizer } from "@tanstack/react-virtual";
import { BatchCache } from "./batchCache";
import { afterNextPaint, registerScroller, signal, type SignalName } from "./bench";

/** Rows rendered above and below the viewport. */
const OVERSCAN = 8;
/** Batches kept in memory: 12 x 500 = 6,000 rows. */
const CACHE_BATCHES = 12;

interface Options<T> {
  name: "table" | "logs";
  count: number;
  rowHeight: number;
  batchSize: number;
  fetchBatch: (start: number, count: number) => Promise<T[]>;
  readySignal: SignalName;
}

function ensureVisible<T>(
  virtualizer: Virtualizer<HTMLDivElement, Element>,
  cache: BatchCache<T>,
): void {
  const items = virtualizer.getVirtualItems();
  if (items.length > 0) cache.ensure(items[0].index, items[items.length - 1].index);
}

export function useBatchedList<T>(options: Options<T>) {
  const { name, count, rowHeight, batchSize, fetchBatch, readySignal } = options;
  const scrollRef = useRef<HTMLDivElement>(null);
  const [, setVersion] = useState(0);
  const [cache] = useState(
    () =>
      new BatchCache<T>(fetchBatch, count, batchSize, CACHE_BATCHES, () =>
        setVersion((version) => version + 1),
      ),
  );
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: OVERSCAN,
    // Called on every scroll: ask for the new rows as early as possible.
    onChange: (instance) => ensureVisible(instance, cache),
  });
  const items = virtualizer.getVirtualItems();

  // After every render: load what is visible (no-op when loaded), and
  // signal the bench once the first real rows have been painted.
  useEffect(() => {
    ensureVisible(virtualizer, cache);
    if (items.length > 0 && cache.get(items[0].index) !== undefined) {
      afterNextPaint(() => signal(readySignal));
    }
  });

  useEffect(() => {
    const element = scrollRef.current;
    if (element === null) return;
    return registerScroller(name, {
      element,
      hasMissingRows: () => {
        const top = element.scrollTop;
        const bottom = top + element.clientHeight;
        return virtualizer
          .getVirtualItems()
          .some(
            (item) =>
              item.end > top && item.start < bottom && cache.get(item.index) === undefined,
          );
      },
    });
  }, [name, virtualizer, cache]);

  return { scrollRef, virtualizer, items, cache };
}
