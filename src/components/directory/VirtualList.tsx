import { useLayoutEffect, useRef, useState, type ReactNode } from "react";

type VirtualListProps<T> = {
  items: T[];
  resetKey: string;
  heightOf: (item: T) => number;
  itemKey: (item: T) => string;
  renderItem: (item: T) => ReactNode;
};

export function VirtualList<T>({ items, resetKey, heightOf, itemKey, renderItem }: VirtualListProps<T>) {
  const scroller = useRef<HTMLDivElement>(null);
  const resetSeen = useRef(resetKey);
  const [range, setRange] = useState({ start: 0, end: 16 });
  const heights = items.map(heightOf);
  const offsets: number[] = new Array(items.length + 1);
  offsets[0] = 0;
  for (let index = 0; index < heights.length; index += 1) {
    offsets[index + 1] = offsets[index] + heights[index];
  }
  const total = offsets[offsets.length - 1] ?? 0;
  const offsetsRef = useRef(offsets);
  offsetsRef.current = offsets;

  useLayoutEffect(() => {
    const element = scroller.current;
    if (!element) {
      return;
    }
    if (resetSeen.current !== resetKey) {
      resetSeen.current = resetKey;
      element.scrollTop = 0;
    }
    const measure = () => {
      const next = visibleRange(offsetsRef.current, element.scrollTop, element.clientHeight);
      setRange((prev) => (prev.start === next.start && prev.end === next.end ? prev : next));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    element.addEventListener("scroll", measure, { passive: true });
    return () => {
      observer.disconnect();
      element.removeEventListener("scroll", measure);
    };
  }, [resetKey, total, items.length]);

  const start = Math.min(range.start, items.length);
  const end = Math.min(range.end, items.length);

  return (
    <div ref={scroller} className="plaza-scroll h-full overflow-y-auto">
      <div className="relative" style={{ height: total }}>
        {items.slice(start, end).map((item, index) => {
          const actual = start + index;
          return (
            <div key={itemKey(item)} className="absolute inset-x-0" style={{ top: offsets[actual], height: heights[actual] }}>
              {renderItem(item)}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function visibleRange(offsets: number[], scrollTop: number, height: number) {
  const count = Math.max(0, offsets.length - 1);
  if (count === 0) {
    return { start: 0, end: 0 };
  }
  const start = Math.max(0, findStart(offsets, scrollTop) - 6);
  const end = Math.min(count, findEnd(offsets, scrollTop + Math.max(height, 1)) + 6);
  return { start, end };
}

function findStart(offsets: number[], top: number) {
  let low = 0;
  let high = offsets.length - 1;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (offsets[mid + 1] <= top) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  return low;
}

function findEnd(offsets: number[], bottom: number) {
  let low = 0;
  let high = offsets.length - 1;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (offsets[mid] < bottom) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  return low;
}
