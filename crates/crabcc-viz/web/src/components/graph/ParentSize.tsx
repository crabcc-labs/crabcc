// Drop-in replacement for `@visx/responsive`'s `<ParentSize>`. We
// only ever used `debounceTime={0}`, so we don't need lodash/debounce
// or the rest of the @visx/responsive surface. Replacing the import
// shrinks the bundle by ~30 KB raw input (lodash + visx + the
// internal hierarchy of HOCs the package ships).
//
// Usage matches the previous API:
//   <ParentSize>{({ width, height }) => …}</ParentSize>

import { useEffect, useRef, useState } from "react";

interface Props {
  children: (size: { width: number; height: number }) => React.ReactNode;
}

export function ParentSize({ children }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    // Initial measurement — the ResizeObserver only fires on the
    // *next* tick, but we want the children to render with real
    // dimensions on first paint where possible.
    const r = el.getBoundingClientRect();
    setSize({ width: r.width, height: r.height });

    const ro = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      // Prefer contentRect (matches getBoundingClientRect's content
      // box on the parent <div>). Newer browsers expose contentBoxSize
      // but contentRect is universally supported.
      const { width, height } = entry.contentRect;
      setSize({ width, height });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  return (
    <div ref={ref} style={{ width: "100%", height: "100%" }}>
      {size.width > 0 && size.height > 0 ? children(size) : null}
    </div>
  );
}
