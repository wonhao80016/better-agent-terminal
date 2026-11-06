import { useCallback, useEffect, useRef, useState, type RefObject } from 'react'

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}

function loadPercent(storageKey: string, defaultPercent: number, min: number, max: number): number {
  try {
    const saved = Number(localStorage.getItem(storageKey))
    if (saved > 0) return clamp(saved, min, max)
  } catch {}
  return defaultPercent
}

export function useResizablePaneWidth(
  containerRef: RefObject<HTMLElement | null>,
  storageKey: string,
  defaultPercent: number,
  min: number,
  max: number,
) {
  const [percent, setPercent] = useState(() => loadPercent(storageKey, defaultPercent, min, max))
  const percentRef = useRef(percent)

  useEffect(() => {
    percentRef.current = percent
  }, [percent])

  const persist = useCallback((value: number) => {
    try { localStorage.setItem(storageKey, String(value)) } catch {}
  }, [storageKey])

  const resize = useCallback((deltaPx: number) => {
    const containerWidth = containerRef.current?.clientWidth
    if (!containerWidth) return
    const next = clamp(percentRef.current + (deltaPx / containerWidth) * 100, min, max)
    percentRef.current = next
    setPercent(next)
    persist(next)
  }, [containerRef, min, max, persist])

  const reset = useCallback(() => {
    percentRef.current = defaultPercent
    setPercent(defaultPercent)
    try { localStorage.removeItem(storageKey) } catch {}
  }, [defaultPercent, storageKey])

  const style = { flex: `0 0 ${percent}%`, maxWidth: `${percent}%` }

  return { style, resize, reset }
}
