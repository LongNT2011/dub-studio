import { useEffect, useRef, useState } from "react";
import { api, type HwSnapshot } from "../lib/api";

const gb = (n: number) => n / 1024 ** 3;
const fmtGb = (n: number) => `${gb(n).toFixed(1)} ГБ`;

// цвет по загрузке: зелёный -> янтарь -> красный
function heat(pct: number): string {
  if (pct >= 90) return "var(--color-danger, #ef4444)";
  if (pct >= 70) return "var(--color-warn, #f59e0b)";
  return "var(--color-accent, #22d3ee)";
}

function Bar({ label, pct, right }: { label: string; pct: number; right: string }) {
  const p = Math.max(0, Math.min(100, pct));
  return (
    <div>
      <div className="flex items-baseline justify-between text-[11px] mb-1">
        <span className="text-[var(--color-muted)]">{label}</span>
        <span className="mono tabular-nums text-[var(--color-fg,#e5e7eb)]">{right}</span>
      </div>
      <div className="h-1.5 rounded-full bg-white/8 overflow-hidden">
        <div
          className="h-full rounded-full transition-[width] duration-500 ease-out"
          style={{ width: `${p}%`, background: heat(p), boxShadow: `0 0 8px ${heat(p)}` }}
        />
      </div>
    </div>
  );
}

export default function ResourceMonitor() {
  const [hw, setHw] = useState<HwSnapshot | null>(null);
  const [open, setOpen] = useState(true);
  const [ok, setOk] = useState(true);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;
    const poll = async () => {
      try {
        const s = await api.hwSnapshot();
        if (alive) { setHw(s); setOk(true); }
      } catch {
        if (alive) setOk(false);
      }
    };
    poll();
    timer.current = window.setInterval(poll, 1000);
    return () => { alive = false; if (timer.current) window.clearInterval(timer.current); };
  }, []);

  if (!ok || !hw || (!hw.gpuName && hw.totalRam === 0)) return null;

  const vramPct = hw.totalVram ? (hw.usedVram / hw.totalVram) * 100 : 0;
  const ramPct = hw.totalRam ? (hw.usedRam / hw.totalRam) * 100 : 0;
  const powerPct = hw.powerLimit ? (hw.powerDraw / hw.powerLimit) * 100 : 0;
  const hasGpu = !!hw.gpuName;

  return (
    <div className="fixed bottom-9 right-4 z-40 select-none" style={{ width: open ? 248 : "auto" }}>
      <div className="rounded-xl border border-white/10 bg-[var(--color-panel,rgba(18,20,26,0.92))] backdrop-blur-md shadow-[0_8px_30px_rgba(0,0,0,0.45)] overflow-hidden">
        <button
          onClick={() => setOpen((v) => !v)}
          className="w-full flex items-center gap-2 px-3 py-2 text-left hover:bg-white/5"
          title={hw.gpuName || "Ресурсы"}
        >
          <span
            className="w-1.5 h-1.5 rounded-full"
            style={{ background: heat(hasGpu ? hw.gpuUtilization : ramPct), boxShadow: `0 0 8px ${heat(hasGpu ? hw.gpuUtilization : ramPct)}` }}
          />
          <span className="text-[12px] font-semibold truncate flex-1">
            {hasGpu ? hw.gpuName.replace(/NVIDIA GeForce /i, "") : "Ресурсы"}
          </span>
          {hasGpu && <span className="mono text-[11px] text-[var(--color-muted)]">{Math.round(hw.temperature)}°</span>}
          <span className="text-[var(--color-muted)] text-[10px]">{open ? "▾" : "▸"}</span>
        </button>

        {open && (
          <div className="px-3 pb-3 pt-1 space-y-2.5">
            {hasGpu && (
              <>
                <Bar label="GPU" pct={hw.gpuUtilization} right={`${Math.round(hw.gpuUtilization)}%`} />
                <Bar label="VRAM" pct={vramPct} right={`${fmtGb(hw.usedVram)} / ${fmtGb(hw.totalVram)}`} />
                <div className="flex items-center justify-between text-[11px]">
                  <span className="text-[var(--color-muted)]">Питание</span>
                  <span className="mono tabular-nums">{Math.round(hw.powerDraw)} / {Math.round(hw.powerLimit)} Вт</span>
                </div>
                <div className="h-1 rounded-full bg-white/8 overflow-hidden">
                  <div className="h-full rounded-full transition-[width] duration-500" style={{ width: `${Math.min(100, powerPct)}%`, background: heat(powerPct) }} />
                </div>
              </>
            )}
            <Bar label="RAM" pct={ramPct} right={`${fmtGb(hw.usedRam)} / ${fmtGb(hw.totalRam)}`} />
            <div className="flex items-center justify-between text-[10px] text-[var(--color-muted)]">
              <span>Процесс</span>
              <span className="mono tabular-nums">{fmtGb(hw.processRam)}</span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
