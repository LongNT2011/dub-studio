import { useCallback, useEffect, useRef, useState } from "react";

export type Floatable = {
  floating: boolean;
  pos: { x: number; y: number };
  pop: () => void;   // оторвать в float
  dock: () => void;  // вернуть в шапку
  dragging: boolean;
  onDragStart: (e: React.PointerEvent) => void;
};

// Докнут в шапке ↔ оторван во float и таскается по всему окну. Позиция и режим — в localStorage.
export function useFloatable(key: string, initial: { x: number; y: number }): Floatable {
  const [floating, setFloating] = useState<boolean>(() => localStorage.getItem(`fl:${key}:on`) === "1");
  const [pos, setPos] = useState<{ x: number; y: number }>(() => {
    try { const s = localStorage.getItem(`fl:${key}:pos`); if (s) return JSON.parse(s); } catch { /* ignore */ }
    return initial;
  });
  const [dragging, setDragging] = useState(false);
  const off = useRef({ x: 0, y: 0 });

  useEffect(() => { localStorage.setItem(`fl:${key}:on`, floating ? "1" : "0"); }, [key, floating]);
  useEffect(() => { localStorage.setItem(`fl:${key}:pos`, JSON.stringify(pos)); }, [key, pos]);

  const onDragStart = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0) return;
    off.current = { x: e.clientX - pos.x, y: e.clientY - pos.y };
    setDragging(true);
    e.preventDefault();
  }, [pos]);

  useEffect(() => {
    if (!dragging) return;
    const move = (e: PointerEvent) => {
      const x = Math.max(4, Math.min(window.innerWidth - 60, e.clientX - off.current.x));
      const y = Math.max(4, Math.min(window.innerHeight - 40, e.clientY - off.current.y));
      setPos({ x, y });
    };
    const up = () => setDragging(false);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    return () => { window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up); };
  }, [dragging]);

  return {
    floating, pos, dragging,
    pop: () => setFloating(true),
    dock: () => setFloating(false),
    onDragStart,
  };
}

// Слот в шапке для докнутых баров (порталим сюда).
export function dockSlot(): HTMLElement | null {
  return typeof document !== "undefined" ? document.getElementById("dock-slot") : null;
}
