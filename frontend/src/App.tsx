import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "motion/react";
import { Upload, Languages, AudioLines, Sparkles, ArrowRight, ShieldCheck, Download, Loader2, Trash2, Plus, Captions, Columns2, FolderDown, ExternalLink, X, Undo2, Redo2, Settings, Eye, EyeOff, Play, Pause, RotateCw, RefreshCw, Square, Droplet, Check, HelpCircle, Copy, Star, Music, Move, Minimize2, FileText, Users, Mic2, AlignLeft, AlignCenter, AlignRight, ChevronFirst, ChevronLast, ArrowLeftToLine, ArrowRightToLine, ChevronDown, ScrollText } from "lucide-react";
import { createPortal } from "react-dom";
import { useFloatable, dockSlot } from "./lib/useFloatable";
import { api, type Project, type Capabilities, type SetupStatus, type SetupComponent } from "./lib/api";
import { LANGS, setLang, type Lang } from "./lib/i18n";
import { useStore } from "./store";
import PreviewCanvas from "./components/PreviewCanvas";
import { playSfx, sfxEnabled, setSfxEnabled } from "./lib/sfx";
import ResourceMonitor from "./components/ResourceMonitor";

const EASE = [0.22, 1, 0.36, 1] as const;

// timecode m:ss.d — the navigation reference shown on every segment + the scrub readout
function fmtT(s: number) {
  const m = Math.floor(s / 60), sec = Math.floor(s % 60), d = Math.floor((s * 10) % 10);
  return `${m}:${String(sec).padStart(2, "0")}.${d}`;
}

function LanguageSwitcher() {
  const { i18n } = useTranslation();
  return (
    <select
      value={i18n.language as Lang}
      onChange={(e) => setLang(e.target.value as Lang)}
      className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-md px-2 py-1 text-sm"
    >
      {LANGS.map((l) => <option key={l} value={l}>{l.toUpperCase()}</option>)}
    </select>
  );
}

// Модели и компоненты в настройках: список из /setup/status с кнопками скачки/докачки и прогрессом.
function ModelsSection() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [prog, setProg] = useState<{ id: string; pct: number } | null>(null);
  const refresh = () => api.setupStatus().then(setStatus).catch(() => {});
  useEffect(() => { refresh(); }, []);
  const dl = async (id: string) => {
    if (prog) return;
    setProg({ id, pct: 0 });
    try {
      const { job_id } = await api.setupDownload([id]);
      await api.watchJob(job_id, (e) => { if (e.type === "progress" && (e.component === id || !e.component)) setProg({ id, pct: e.pct ?? 0 }); });
    } catch { /* ignore */ } finally { setProg(null); await refresh(); }
  };
  const browseId = async (id: string) => {
    if (prog) return;
    try { const r = await api.setupBrowse(id); if (r.picked) setStatus(r.status); } catch { /* ignore */ }
  };
  const BrowseBtn = ({ id }: { id: string }) => (
    <button onClick={() => browseId(id)} disabled={!!prog} title={t("settings.browseFolder")}
      className="shrink-0 inline-flex items-center px-1.5 py-1 rounded-md border border-[var(--color-border)] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)] disabled:opacity-40">
      <FolderDown size={12} />
    </button>
  );
  if (!status) return <div className="mono text-[11px] text-[var(--color-muted)]">…</div>;
  const byId = Object.fromEntries(status.components.map((c) => [c.id, c]));
  const get = (id: string) => byId[id] as SetupComponent | undefined;

  // строка одного компонента (кнопка скачать/докачать + прогресс)
  const Row = (c: SetupComponent) => {
    const active = prog?.id === c.id;
    return (
      <div key={c.id} className="flex items-center gap-2.5 px-2.5 py-2 rounded-lg bg-[var(--color-surface-2)] border border-[var(--color-border)]">
        <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${c.installed ? "bg-[var(--color-accent)]" : "bg-[var(--color-muted)]"}`} />
        <div className="min-w-0 flex-1">
          <div className="text-[12px] font-medium truncate">{c.name}</div>
          <div className="mono text-[10px] text-[var(--color-muted)] truncate">{c.purpose} · {c.vram ? `${fmtBytes(c.vram)} VRAM · ` : ""}{fmtBytes(c.size)} {t("settings.disk")}</div>
        </div>
        {active ? (
          <div className="w-24 shrink-0">
            <div className="h-1.5 rounded-full bg-white/10 overflow-hidden"><div className="h-full bg-[var(--color-accent)] transition-[width]" style={{ width: `${prog?.pct ?? 0}%` }} /></div>
            <div className="mono text-[10px] text-[var(--color-muted)] text-right mt-0.5">{Math.round(prog?.pct ?? 0)}%</div>
          </div>
        ) : (
          <div className="flex items-center gap-1.5 shrink-0">
            <BrowseBtn id={c.id} />
            <button onClick={() => dl(c.id)} disabled={!!prog}
              className="inline-flex items-center gap-1 px-2.5 py-1 rounded-md text-[12px] border border-[var(--color-border)] text-[var(--color-accent-2)] hover:border-[var(--color-accent)] disabled:opacity-40">
              {c.installed ? <RefreshCw size={12} /> : <Download size={12} />}{c.installed ? t("settings.redownload") : t("setup.downloadOne")}
            </button>
          </div>
        )}
      </div>
    );
  };

  // модель с выбором кванта: дропдаун вариантов + скачать выбранный
  const VariantPicker = ({ base, ids }: { base: string; ids: string[] }) => {
    const variants = ids.map(get).filter(Boolean) as SetupComponent[];
    const installed = variants.find((v) => v.installed);
    const [pick, setPick] = useState<string>(installed?.id ?? variants[0]?.id ?? "");
    const c = get(pick);
    if (!c) return null;
    const quant = (v: SetupComponent) => (v.name.match(/\b(q\d[\w]*|int8|fp32|f16)\b/i)?.[1] ?? v.name);
    const active = prog?.id === c.id;
    return (
      <div className="px-2.5 py-2 rounded-lg bg-[var(--color-surface-2)] border border-[var(--color-border)]">
        <div className="flex items-center gap-2.5">
          <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${variants.some((v) => v.installed) ? "bg-[var(--color-accent)]" : "bg-[var(--color-muted)]"}`} />
          <div className="min-w-0 flex-1">
            <div className="text-[12px] font-medium truncate">{base}</div>
            <div className="mono text-[10px] text-[var(--color-muted)] truncate">{c.vram ? `${fmtBytes(c.vram)} VRAM · ` : ""}{fmtBytes(c.size)} {t("settings.disk")}</div>
          </div>
          <select value={pick} onChange={(e) => { const id = e.target.value; setPick(id); const sv = get(id); if (sv?.installed) api.selectModel(id).catch(() => {}); }}
            className="shrink-0 bg-[var(--color-surface)] border border-[var(--color-border)] rounded-md px-2 py-1 text-[11px] mono focus:border-[var(--color-accent)] focus:outline-none">
            {variants.map((v) => <option key={v.id} value={v.id}>{quant(v)}{v.installed ? " ✓" : ""} · {fmtBytes(v.size)}{v.vram ? ` / ${fmtBytes(v.vram)} VRAM` : ""}</option>)}
          </select>
          {active ? (
            <span className="mono text-[11px] text-[var(--color-accent)] w-10 text-right">{Math.round(prog?.pct ?? 0)}%</span>
          ) : (
            <>
              <BrowseBtn id={c.id} />
              <button onClick={() => dl(c.id)} disabled={!!prog} title={c.installed ? t("settings.redownload") : t("setup.downloadOne")}
                className="shrink-0 inline-flex items-center gap-1 px-2 py-1 rounded-md text-[12px] border border-[var(--color-border)] text-[var(--color-accent-2)] hover:border-[var(--color-accent)] disabled:opacity-40">
                {c.installed ? <RefreshCw size={12} /> : <Download size={12} />}
              </button>
            </>
          )}
        </div>
      </div>
    );
  };

  const Group = ({ label, children }: { label: string; children: React.ReactNode }) => (
    <div className="mb-3">
      <div className="text-[11px] uppercase tracking-[0.14em] text-[var(--color-muted)] mb-1.5">{label}</div>
      <div className="space-y-1.5">{children}</div>
    </div>
  );

  const rowOf = (id: string) => { const c = get(id); return c ? <Row key={id} {...c} /> : null; };

  const browse = async () => {
    if (prog) return;
    try { const r = await api.setupBrowse(); if (r.picked) setStatus(r.status); } catch { /* ignore */ }
  };

  return (
    <div>
      <button onClick={browse} disabled={!!prog}
        className="mb-3 w-full inline-flex items-center justify-center gap-2 px-3 py-2 rounded-lg border border-dashed border-[var(--color-border)] text-[12px] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)] transition-colors disabled:opacity-40">
        <FolderDown size={14} />{t("settings.browseFolder")}
      </button>
      <Group label={t("settings.roleTts")}>
        <VariantPicker base="Higgs Audio v3" ids={["higgs", "higgs-q6_k", "higgs-q4_k_m"]} />
        {rowOf("higgs-engine")}
      </Group>
      <Group label={t("settings.roleAsr")}>
        <VariantPicker base="Parakeet-TDT 0.6B v3" ids={["parakeet", "parakeet-fp32"]} />
      </Group>
      <Group label={t("settings.roleMt")}>
        <VariantPicker base="Gemma-4 12B QAT + vision" ids={["gemma", "gemma-q5_0", "gemma-q6_k", "gemma-q8_0"]} />
        {rowOf("llama")}
      </Group>
      <Group label={t("settings.roleSep")}>
        <VariantPicker base="Mel-Band Roformer voc_fv6" ids={["roformer", "roformer-q5", "roformer-q4"]} />
        {rowOf("bsroformer-engine")}
      </Group>
      <Group label={t("settings.roleDiar")}>{rowOf("sortformer")}</Group>
      <Group label={t("settings.roleRuntime")}>{rowOf("onnxruntime")}{rowOf("ffmpeg")}{rowOf("cuda-runtime")}{rowOf("vcruntime")}{rowOf("ocr")}</Group>
    </div>
  );
}

function SettingsModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [cap, setCap] = useState<Capabilities | null>(null);
  const [sfx, setSfx] = useState(sfxEnabled());
  useEffect(() => { api.capabilities().then(setCap).catch(() => {}); }, []);
  return (
    <div className="fixed inset-0 z-50 grid place-items-center glass-scrim anim-fade" onClick={onClose}>
      <div className="w-[min(92vw,600px)] max-h-[86vh] flex flex-col rounded-xl glass-panel anim-pop p-5" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-1">
          <span className="font-semibold">{t("settings.title")}</span>
          <button onClick={onClose} className="text-[var(--color-muted)] hover:text-[var(--color-text)]"><X size={16} /></button>
        </div>
        <div className="mono text-[11px] text-[var(--color-muted)] mb-3">{cap ? `${cap.device} · ${cap.tts_quant}` : "…"}</div>
        <label className="flex items-center justify-between gap-3 mb-3 pb-3 border-b border-[var(--color-border)]">
          <span className="text-[13px] text-[var(--color-text)] inline-flex items-center gap-2"><Music size={14} className="text-[var(--color-muted)]" />{t("settings.sounds")}</span>
          <button onClick={() => { const v = !sfx; setSfx(v); setSfxEnabled(v); if (v) playSfx("notify"); }} title={t("settings.sounds")}
            className={`relative w-9 h-5 rounded-full transition-colors shrink-0 ${sfx ? "bg-[var(--color-accent)]" : "bg-[var(--color-surface-2)] border border-[var(--color-border)]"}`}>
            <span className={`absolute top-0.5 w-4 h-4 rounded-full bg-white transition-all ${sfx ? "left-[18px]" : "left-0.5"}`} />
          </button>
        </label>
        <div className="overflow-y-auto flex-1 -mr-2 pr-2"><ModelsSection /></div>
      </div>
    </div>
  );
}

const DONATE = {
  boosty: "https://boosty.to/neuro_art",
  dalink: "https://dalink.to/nerual_dreming",
  github: "https://github.com/timoncool/dub-studio",
  telegram: "https://t.me/nerual_dreming",
  crypto: [["BTC", "1E7dHL22RpyhJGVpcvKdbyZgksSYkYeEBC"],
           ["ETH · ERC20", "0xb5db65adf478983186d4897ba92fe2c25c594a0c"],
           ["USDT · TRC20", "TQST9Lp2TjK6FiVkn4fwfGUee7NmkxEE7C"]] as const,
};

function HelpSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mt-4">
      <div className="text-[11px] uppercase tracking-[0.16em] text-[var(--color-muted)] mb-1.5">{title}</div>
      {children}
    </div>
  );
}

function CryptoRow({ coin, addr }: { coin: string; addr: string }) {
  const { t } = useTranslation();
  const [done, setDone] = useState(false);
  return (
    <button title={t("help.copy")}
      onClick={async () => { try { await navigator.clipboard.writeText(addr); setDone(true); setTimeout(() => setDone(false), 1200); } catch { /* clipboard blocked */ } }}
      className="w-full flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-[var(--color-surface-2)] border border-[var(--color-border)] hover:border-[var(--color-accent)] transition-colors text-left">
      <span className="text-[11px] uppercase tracking-[0.1em] text-[var(--color-muted)] w-[92px] shrink-0">{coin}</span>
      <span className="mono text-[11px] truncate flex-1">{addr}</span>
      {done ? <Check size={13} className="text-[var(--color-accent)] shrink-0" /> : <Copy size={13} className="text-[var(--color-muted)] shrink-0" />}
    </button>
  );
}

function HelpModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const how = t("help.how", { returnObjects: true }) as unknown as string[];
  const features = t("help.features", { returnObjects: true }) as unknown as string[];
  const sections = t("help.sections", { returnObjects: true }) as unknown as string[];
  const pay = "flex items-center justify-center gap-2 px-3 py-2 rounded-lg bg-[var(--color-surface-2)] border border-[var(--color-border)] text-[13px] font-medium text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors";
  const chip = "inline-flex items-center gap-1 px-2 py-1 rounded-md bg-[var(--color-surface-2)] border border-[var(--color-border)] text-[11px] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)] transition-colors";
  return (
    <div className="fixed inset-0 z-50 grid place-items-center glass-scrim anim-fade" onClick={onClose}>
      <div className="w-[min(92vw,640px)] max-h-[86vh] overflow-y-auto rounded-xl glass-panel anim-pop p-5" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-2">
          <span className="flex items-center gap-2 font-semibold"><HelpCircle size={17} className="text-[var(--color-accent)]" />{t("help.title")}</span>
          <button onClick={onClose} className="text-[var(--color-muted)] hover:text-[var(--color-text)]"><X size={16} /></button>
        </div>
        <p className="text-[13px] leading-relaxed text-[var(--color-muted)]">{t("help.intro")}</p>

        <HelpSection title={t("help.howTitle")}>
          <ol className="space-y-1.5">
            {how.map((s, i) => (
              <li key={i} className="flex gap-2.5 text-[13px]">
                <span className="grid place-items-center w-5 h-5 shrink-0 rounded-full bg-[var(--color-surface-2)] text-[var(--color-accent)] text-[11px] font-semibold">{i + 1}</span>
                <span className="leading-relaxed">{s}</span>
              </li>
            ))}
          </ol>
        </HelpSection>

        <HelpSection title={t("help.featuresTitle")}>
          <ul className="grid sm:grid-cols-2 gap-x-4 gap-y-1.5">
            {features.map((f, i) => (
              <li key={i} className="flex gap-2 text-[13px] leading-relaxed"><Check size={14} className="text-[var(--color-accent)] shrink-0 mt-[3px]" /><span>{f}</span></li>
            ))}
          </ul>
        </HelpSection>

        <HelpSection title={t("help.sectionsTitle")}>
          <ul className="space-y-1">
            {sections.map((s, i) => <li key={i} className="text-[13px] leading-relaxed text-[var(--color-muted)]">· {s}</li>)}
          </ul>
        </HelpSection>

        <HelpSection title={t("help.donateTitle")}>
          <p className="text-[13px] leading-relaxed text-[var(--color-muted)] mb-3">{t("help.donateIntro")}</p>
          <div className="grid sm:grid-cols-2 gap-2 mb-2.5">
            <a href={DONATE.dalink} target="_blank" rel="noreferrer" className={pay}>💳 {t("help.card")}</a>
            <a href={DONATE.boosty} target="_blank" rel="noreferrer" className={pay}>🚀 {t("help.boostySub")}</a>
          </div>
          <div className="space-y-1.5">
            {DONATE.crypto.map(([c, a]) => <CryptoRow key={c} coin={c} addr={a} />)}
          </div>
          <div className="mt-4 pt-3 border-t border-[var(--color-border)] text-[12px] leading-relaxed text-[var(--color-muted)]">
            {t("help.madeBy")} <a className="text-[var(--color-text)] hover:text-[var(--color-accent)] transition-colors" href={DONATE.telegram} target="_blank" rel="noreferrer">Nerual Dreming</a> — {t("help.founder")} <a className="text-[var(--color-text)] hover:text-[var(--color-accent)] transition-colors" href="https://artgeneration.me" target="_blank" rel="noreferrer">ArtGeneration.me</a>
            <div className="flex flex-wrap gap-1.5 mt-2">
              <a href="https://t.me/neuroport" target="_blank" rel="noreferrer" className={chip}>Нейро-Софт</a>
              <a href={DONATE.github} target="_blank" rel="noreferrer" className={chip}><Star size={11} />GitHub</a>
              <a href={DONATE.telegram} target="_blank" rel="noreferrer" className={chip}>Telegram</a>
            </div>
          </div>
        </HelpSection>
      </div>
    </div>
  );
}

// Статус-строка в шапке: текущее действие приложения + разворот по клику в полный журнал (что делалось).
function StatusBar() {
  const { t } = useTranslation();
  const activities = useStore((s) => s.activities);
  const progress = useStore((s) => s.progress);
  const rendering = useStore((s) => s.rendering);
  const [open, setOpen] = useState(false);
  const last = activities[activities.length - 1];
  const busy = rendering || progress.pct != null || (!!progress.stage && !["", "done", "error"].includes(progress.stage));
  const text = busy ? (stageLabel(progress.stage, t) || progress.msg || last?.text || t("status.working")) : (last?.text || t("status.idle"));
  const errored = !busy && last?.kind === "error";
  const fmt = (ms: number) => new Date(ms).toLocaleTimeString();
  return (
    <div className="relative flex-1 min-w-0 flex justify-center px-3">
      <button onClick={() => setOpen((o) => !o)} title={t("status.log")}
        className="inline-flex items-center gap-2 max-w-full px-3 py-1 rounded-md text-[12px] text-[var(--color-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-surface-2)] transition-colors">
        {busy
          ? <Loader2 size={13} className="animate-spin text-[var(--color-accent)] shrink-0" />
          : <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${errored ? "bg-[var(--color-warn)]" : "bg-[var(--color-accent)]"}`} />}
        <span className="truncate">{text}</span>
        {activities.length > 0 && <ChevronDown size={13} className={`shrink-0 transition-transform ${open ? "rotate-180" : ""}`} />}
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
          <div className="absolute top-full mt-1 left-1/2 -translate-x-1/2 z-50 w-[min(560px,92vw)] max-h-[60vh] overflow-y-auto rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] shadow-xl p-1.5">
            <div className="flex items-center justify-between px-2 py-1 mb-1 border-b border-[var(--color-border)]">
              <span className="text-[11px] uppercase tracking-wide text-[var(--color-muted)] inline-flex items-center gap-1.5"><ScrollText size={13} />{t("status.log")}</span>
              {activities.length > 0 && <button onClick={() => useStore.setState({ activities: [] })} className="text-[11px] text-[var(--color-muted)] hover:text-[var(--color-text)] transition-colors">{t("status.clear")}</button>}
            </div>
            {activities.length === 0 && <div className="text-[11px] text-[var(--color-muted)]/60 py-3 text-center">—</div>}
            {[...activities].reverse().map((a, i) => (
              <div key={i} className="flex items-start gap-2 px-2 py-1 text-[12px]">
                <span className="mono text-[10px] text-[var(--color-muted)]/70 tabnum shrink-0 mt-[3px]">{fmt(a.t)}</span>
                <span className={`w-1.5 h-1.5 rounded-full shrink-0 mt-1.5 ${a.kind === "error" ? "bg-[var(--color-warn)]" : a.kind === "done" ? "bg-[var(--color-accent)]" : "bg-[var(--color-muted)]"}`} />
                <span className={`leading-snug break-words min-w-0 ${a.kind === "error" ? "text-[var(--color-warn)]" : "text-[var(--color-text)]"}`}>{a.text}</span>
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

function TopBar() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState(false);
  const [help, setHelp] = useState(false);
  const stage = useStore((s) => s.stage);
  const setStage = useStore((s) => s.setStage);
  const setPid = useStore((s) => s.setPid);
  const setProject = useStore((s) => s.setProject);
  // start over with a new video — the current project stays on disk (reachable via Recent), so no confirm needed
  const newProject = () => {
    setProject(null); setPid(null); setStage("empty");
    try { history.replaceState(null, "", location.pathname); } catch { /* no-op */ }
  };
  return (
    <header className="flex items-center justify-between px-5 h-14 border-b border-[var(--color-border)] bg-[var(--color-surface)]">
      <div className="flex items-center gap-3">
        <span className="flex items-center gap-2 font-bold tracking-tight">
          <img src="/favicon.svg" alt="" width={20} height={20} className="rounded-[5px] shadow-[0_0_10px_rgba(198,242,78,0.25)]" />
          {t("app.name")}
        </span>
      </div>
      <StatusBar />
      <div className="flex items-center gap-2">
        <div id="dock-slot" className="flex items-center gap-2" />
        {stage !== "empty" && (
          <button onClick={newProject} title={t("nav.newHint")}
            className="inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-[13px] font-medium text-[var(--color-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-surface-2)] transition-colors">
            <Plus size={16} /><span className="hidden sm:inline">{t("nav.new")}</span>
          </button>
        )}
        <button onClick={() => setHelp(true)} title={t("help.title")}
          className="p-1.5 rounded-md text-[var(--color-muted)] hover:text-[var(--color-text)] transition-colors"><HelpCircle size={18} /></button>
        <button onClick={() => setSettings(true)} title={t("settings.title")}
          className="p-1.5 rounded-md text-[var(--color-muted)] hover:text-[var(--color-text)] transition-colors"><Settings size={18} /></button>
        <LanguageSwitcher />
      </div>
      {help && <HelpModal onClose={() => setHelp(false)} />}
      {settings && <SettingsModal onClose={() => setSettings(false)} />}
    </header>
  );
}

function Feature({ icon: Icon, title, desc, delay }: { icon: typeof Languages; title: string; desc: string; delay: number }) {
  return (
    <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.45, ease: EASE, delay }}
      className="flex items-start gap-3.5">
      <div className="mt-0.5 grid place-items-center w-9 h-9 shrink-0 rounded-lg bg-[var(--color-surface-2)] border border-[var(--color-border)] text-[var(--color-accent)]">
        <Icon size={18} strokeWidth={2} />
      </div>
      <div>
        <div className="font-semibold text-[15px] leading-tight">{title}</div>
        <div className="text-sm text-[var(--color-muted)] mt-0.5">{desc}</div>
      </div>
    </motion.div>
  );
}

// Принимаем все распространённые видео-форматы (ffmpeg декодит их все) — не только mp4/mov.
const VIDEO_ACCEPT = "video/*,.mp4,.mov,.mkv,.avi,.webm,.m4v,.flv,.wmv,.ts,.mpg,.mpeg,.3gp,.ogv,.mts,.m2ts,.vob,.f4v";

function DropZone() {
  const { t, i18n } = useTranslation();
  const s = useStore();
  const inputRef = useRef<HTMLInputElement>(null);
  const batchRef = useRef<HTMLInputElement>(null);
  const [over, setOver] = useState(false);
  const [tgt, setTgt] = useState<string>((i18n.language as string) || "ru");   // translate TO (default = UI lang)
  const [src, setSrc] = useState("auto");                                       // translate FROM (auto-detect)
  const [file, setFile] = useState<File | null>(null);                          // staged video — analyzed on Start, not on drop
  // Композируемые опции обработки (независимы, любые комбинации). audio = аудио-выход; subs = содержимое
  // субтитров; burn = вжигать ли их на видео; funnyOn+funny = шуточный ремикс (сочетается с дубляжом/голосом).
  const [audio, setAudio] = useState<"nodub" | "dub" | "voiceover" | "transcribe">("dub");
  const [subs, setSubs] = useState<"none" | "transcribe" | "translate">("translate");
  const [burn, setBurn] = useState(true);
  const [funnyOn, setFunnyOn] = useState(false);
  const [funny, setFunny] = useState("");                                       // Gemma rewrite instruction (тема ремикса)
  const [preview, setPreview] = useState<string | null>(null);                  // objectURL превью выбранного видео (первый кадр)
  useEffect(() => {                                                             // создаём/освобождаем objectURL под выбранный файл
    if (!file) { setPreview(null); return; }
    const url = URL.createObjectURL(file);
    setPreview(url);
    return () => URL.revokeObjectURL(url);
  }, [file]);

  async function run() {
    if (!file) return;
    s.setStage("analyzing");
    s.setProgress("", "", null);             // fresh stepper for this run
    try {
      const { project_id } = await api.createProject(file);
      s.setPid(project_id);
      // subtitles = ОРИГИНАЛ: исходная дорожка + субтитры на языке оригинала (без дубляжа, без перевода);
      // voiceover = закадровый (перевод+TTS, оригинал слышно приглушённым); transcribe = транскрипт+диаризация.
      // Композируемо: аудио-выход, содержимое субтитров, шуточный ремикс — независимы.
      const eMode = audio;                                          // nodub | dub | voiceover | transcribe
      const eSubs = audio === "transcribe" ? "transcribe" : subs;   // none | transcribe(оригинал) | translate
      const eRewrite = funnyOn && (audio === "dub" || audio === "voiceover") ? funny.trim() : "";
      const { job_id } = await api.analyze(project_id, tgt, eMode, src, eSubs, eRewrite, burn);
      await api.watchJob(job_id, (e) => { if (e.type === "progress") s.setProgress(e.stage || "", e.msg || "", e.pct ?? null); });
      s.setProject(await api.getProject(project_id));
      // Озвучку готовим ЗДЕСЬ, на экране загрузки (не собирая видео — кадры даёт per-frame preview),
      // чтобы редактор открылся с готовым дубом (плей сразу играет). Иначе рендер блокировал бы превью
      // после открытия -> чёрный экран, и слушать дуб можно было бы только после экспорта.
      if (audio === "dub" || audio === "voiceover") {
        try {
          const r = await api.render(project_id);   // полный дубляж на экране ЗАГРУЗКИ (1:1 питон: analyze -> analyzed.mp4): TTS+микс+бёрн+mux -> output.mp4
          await api.watchJob(r.job_id, (e) => { if (e.type === "progress") s.setProgress(e.stage || "voicing", e.msg || "", e.pct ?? null); });
          s.setProject(await api.getProject(project_id));
          // rendered ОСТАЁТСЯ false: покадровое превью <img> (редактирование), а /dub отдаёт готовый дуб
          // (output.mp4) -> плей играет озвучку и двигает скраб -> кадры следуют (1:1 оригинал).
        } catch { /* рендер не удался -> редактор откроется на покадровом превью */ }
      }
      s.setStage("editor"); playSfx("success");
    } catch (err) {
      s.setProgress("error", String(err), null);  // surface backend failure instead of hanging on "analyzing"
      s.setStage("empty"); playSfx("error");
    }
  }

  const corner = `absolute w-4 h-4 rounded-[2px] transition-colors duration-200 ${over ? "border-[var(--color-accent)]" : "border-[var(--color-border)]"}`;
  return (
    <div className="flex-1 min-h-0 overflow-y-auto grid place-items-center px-6 py-10">
      <motion.div initial={{ opacity: 0, y: 14 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.5, ease: EASE }}
        className="w-full max-w-5xl grid lg:grid-cols-[1.05fr_0.95fr] gap-12 items-center">

        <div>
          <div className="mono text-[11px] tracking-[0.2em] text-[var(--color-muted)] flex items-center gap-2">
            <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-accent)] shadow-[0_0_8px_var(--color-accent)]" />{t("hero.eyebrow")}
          </div>
          <h1 className="mt-5 text-3xl lg:text-[36px] leading-[1.06] font-extrabold tracking-tight max-w-xl">{t("hero.headline")}</h1>
          <p className="mt-4 text-[15px] leading-relaxed text-[var(--color-muted)] max-w-lg">{t("hero.sub")}</p>
          {s.progress.stage === "error" && (   // analyze failed -> stage flips back to "empty"; show WHY here instead of silently returning to the drop screen
            <div className="mt-4 max-w-lg rounded-lg border border-[var(--color-warn)]/40 bg-[color-mix(in_oklab,var(--color-warn)_10%,transparent)] px-3 py-2 text-[13px] text-[var(--color-warn)]">
              <span className="font-semibold">{t("common.error")}</span> · <span className="mono text-[11px] break-words">{s.progress.msg}</span>
            </div>
          )}
          <div className="mt-8 space-y-4">
            <Feature icon={AudioLines} title={t("actions.dub")} desc={t("hero.f2")} delay={0.12} />
            <Feature icon={Mic2} title={t("mode.voiceover")} desc={t("mode.voiceover_desc")} delay={0.18} />
            <Feature icon={Captions} title={t("mode.subtitles")} desc={t("mode.subtitles_desc")} delay={0.24} />
            <Feature icon={Sparkles} title={t("actions.funny")} desc={t("hero.f3")} delay={0.3} />
            <Feature icon={FileText} title={t("actions.transcribe")} desc={t("hero.f4")} delay={0.36} />
          </div>
        </div>

        <motion.div initial={{ opacity: 0, scale: 0.98 }} animate={{ opacity: 1, scale: 1 }} transition={{ duration: 0.5, ease: EASE, delay: 0.1 }}>
          <div
            onDragOver={(e) => { e.preventDefault(); setOver(true); }}
            onDragLeave={() => setOver(false)}
            onDrop={(e) => { e.preventDefault(); setOver(false); const f = e.dataTransfer.files?.[0]; if (f) setFile(f); }}
            onClick={() => inputRef.current?.click()}
            className={`group relative aspect-[4/3] rounded-2xl border grid place-items-center cursor-pointer overflow-hidden transition-all duration-200
              ${over ? "border-[var(--color-accent)] bg-[color-mix(in_oklab,var(--color-accent)_9%,var(--color-surface))] shadow-[0_0_0_4px_color-mix(in_oklab,var(--color-accent)_18%,transparent)]"
                     : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-2)] hover:border-[#3a414c]"}`}
          >
            {file && preview && (   // превью выбранного видео (первый кадр) заполняет рамку; скрим — для читаемости текста
              <>
                <video src={preview} muted playsInline preload="metadata" className="absolute inset-0 w-full h-full object-cover" />
                <div className="absolute inset-0 bg-black/45" />
              </>
            )}
            <span className={`${corner} top-3 left-3 border-l border-t`} />
            <span className={`${corner} top-3 right-3 border-r border-t`} />
            <span className={`${corner} bottom-3 left-3 border-l border-b`} />
            <span className={`${corner} bottom-3 right-3 border-r border-b`} />
            <div className="relative z-10 text-center px-6">
              <div className={`mx-auto grid place-items-center w-16 h-16 rounded-2xl border transition-all duration-200
                ${over ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] border-transparent" : file && preview ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] border-transparent shadow-lg" : "bg-[var(--color-surface-2)] text-[var(--color-accent)] border-[var(--color-border)] group-hover:scale-105"}`}>
                {file ? <Check size={26} strokeWidth={2.5} /> : <Upload size={26} strokeWidth={2} />}
              </div>
              {file ? (
                <>
                  <div className={`mt-5 text-lg font-semibold break-all px-2 ${preview ? "text-white drop-shadow" : ""}`}>{file.name}</div>
                  <div className={`mt-1.5 text-sm ${preview ? "text-white/80" : "text-[var(--color-muted)]"}`}>{(file.size / 1048576).toFixed(1)} MB · {t("drop.change")}</div>
                </>
              ) : (
                <>
                  <div className="mt-5 text-lg font-semibold">{t("drop.title")}</div>
                  <div className="mt-1.5 text-sm text-[var(--color-muted)]">{t("drop.hint")}</div>
                  <span className="mt-5 inline-flex items-center gap-2 px-4 py-2 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold">
                    {t("drop.browse")} <ArrowRight size={16} /></span>
                </>
              )}
            </div>
          </div>
          <div className="mt-3.5 flex items-center justify-center gap-2 text-[12px]">
            <Languages size={14} className="text-[var(--color-accent-2)]" />
            <select value={src} onChange={(e) => setSrc(e.target.value)}
              className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none">
              <option value="auto">auto</option>
              {LANGS.map((l) => <option key={l} value={l}>{l.toUpperCase()}</option>)}
            </select>
            <ArrowRight size={12} className="text-[var(--color-muted)]" />
            <select value={tgt} onChange={(e) => setTgt(e.target.value)}
              className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1.5 py-0.5 text-[11px] text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none">
              {LANGS.map((l) => <option key={l} value={l}>{l.toUpperCase()}</option>)}
            </select>
          </div>
          {/* АУДИО-ВЫХОД (независимо от субтитров) */}
          <div className="mt-3 text-[11px] uppercase tracking-[0.1em] text-[var(--color-muted)] mb-1">{t("comp.audioLabel")}</div>
          <div className="grid grid-cols-2 gap-1.5">
            {([["nodub", Captions, "audioNone"], ["dub", AudioLines, "audioDub"], ["voiceover", Mic2, "audioVoiceover"], ["transcribe", FileText, "mode.transcribe"]] as const).map(([a, Icon, key]) => (
              <button key={a} onClick={() => setAudio(a)}
                className={`flex items-center justify-center gap-1.5 px-2 py-2 rounded-lg border text-[12px] font-medium transition-colors ${audio === a ? "border-[var(--color-accent)] bg-[color-mix(in_oklab,var(--color-accent)_12%,transparent)] text-[var(--color-text)]" : "border-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
                <Icon size={14} />{key.includes(".") ? t(key) : t(`comp.${key}`)}</button>
            ))}
          </div>
          {/* ШУТОЧНЫЙ РЕМИКС — сочетается с дубляжом/закадром (и с выбранными голосами в редакторе) */}
          {(audio === "dub" || audio === "voiceover") && (
            <label className="mt-2 flex items-center gap-2 text-[12px] cursor-pointer select-none">
              <input type="checkbox" checked={funnyOn} onChange={(e) => setFunnyOn(e.target.checked)} className="accent-[var(--color-accent)] w-3.5 h-3.5" />
              <Sparkles size={13} className="text-[var(--color-accent-2)]" />{t("comp.funny")}
            </label>
          )}
          {funnyOn && (audio === "dub" || audio === "voiceover") && (
            <textarea value={funny} onChange={(e) => setFunny(e.target.value)} rows={2} placeholder={t("remix.placeholder")}
              className="mt-1.5 w-full bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-lg px-2.5 py-2 text-[12px] focus:border-[var(--color-accent)] focus:outline-none resize-none" />
          )}
          {/* СУБТИТРЫ (независимо от аудио) */}
          {audio !== "transcribe" && (
            <>
              <div className="mt-3 text-[11px] uppercase tracking-[0.1em] text-[var(--color-muted)] mb-1">{t("comp.subsLabel")}</div>
              <div className="grid grid-cols-3 gap-1.5">
                {([["none", "subsNone"], ["transcribe", "subsOriginal"], ["translate", "subsTranslate"]] as const).map(([sv, key]) => (
                  <button key={sv} onClick={() => setSubs(sv)}
                    className={`px-2 py-1.5 rounded-lg border text-[11px] font-medium transition-colors ${subs === sv ? "border-[var(--color-accent)] bg-[color-mix(in_oklab,var(--color-accent)_12%,transparent)] text-[var(--color-text)]" : "border-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
                    {t(`comp.${key}`)}</button>
                ))}
              </div>
              {subs !== "none" && (
                <label className="mt-1.5 flex items-center gap-2 text-[12px] cursor-pointer select-none" title={t("comp.burnHint")}>
                  <input type="checkbox" checked={burn} onChange={(e) => setBurn(e.target.checked)} className="accent-[var(--color-accent)] w-3.5 h-3.5" />
                  {t("comp.burn")}
                </label>
              )}
            </>
          )}
          <button onClick={run} disabled={!file || (funnyOn && (audio === "dub" || audio === "voiceover") && !funny.trim())}
            className="mt-2.5 w-full inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-40 hover:brightness-105 transition">
            {t("drop.start")} <ArrowRight size={16} /></button>
          <button onClick={() => batchRef.current?.click()}
            className="mt-2 w-full inline-flex items-center justify-center gap-2 px-4 py-2 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-muted)] text-[13px] font-medium hover:text-[var(--color-text)] hover:border-[#3a414c] transition">
            <FolderDown size={15} />{t("batch.open")}</button>
          <div className="mt-2 flex items-center justify-center gap-2 text-[12px] text-[var(--color-muted)]">
            <ShieldCheck size={14} className="text-[var(--color-accent-2)]" /> {t("hero.formats")}
          </div>
        </motion.div>
      </motion.div>
      <input ref={inputRef} type="file" accept={VIDEO_ACCEPT} className="hidden"
        onChange={(e) => { const f = e.target.files?.[0]; if (f) setFile(f); }} />
      <input ref={batchRef} type="file" multiple accept={VIDEO_ACCEPT} className="hidden"
        onChange={(e) => { const fs = [...(e.target.files || [])]; if (fs.length) { batchState.files = fs; batchState.tgt = tgt; batchState.src = src; batchState.audio = audio; batchState.subs = subs; batchState.burn = burn; batchState.funnyOn = funnyOn; batchState.funny = funny; s.setStage("batch"); } }} />
    </div>
  );
}

// editor stages mapped to the engine's stage markers (api._run emits `stage` per _timed block + "download").
const ANALYZE_STEPS: { key: string; stages: string[] }[] = [
  { key: "download",    stages: ["download"] },
  { key: "separating",  stages: ["extract_audio", "separate"] },
  { key: "diarizing",   stages: ["diarize"] },
  { key: "recognizing", stages: ["asr"] },
  { key: "translating", stages: ["translate", "translate_ctx", "vision", "rewrite", "rewrite_ctx"] },   // "vision" = ctx-проход (vision layout + перевод транскрипта)
  { key: "voicing",     stages: ["tts", "mix"] },        // TTS synthesis + mix — runs BETWEEN translate and OCR; without this the stepper blanks (cur=-1) during voice gen
  { key: "locating",    stages: ["ocr_detect", "translate_titles", "translate_tagline", "build", "burn", "mux"] },
];
// стадия -> переведённая метка шага (бэкенд шлёт детальный msg по-русски; в UI показываем локализованный
// ярлык стадии вместо сырого текста, чтобы статус был на языке интерфейса). Неизвестная стадия -> null.
const STAGE_TO_STEPKEY: Record<string, string> = Object.fromEntries(
  ANALYZE_STEPS.flatMap((s) => s.stages.map((st) => [st, s.key])),
);
function stageLabel(stage: string | undefined, t: (k: string) => string): string | null {
  if (!stage) return null;
  const k = STAGE_TO_STEPKEY[stage];
  return k ? t(`analyze.${k}`) : null;
}

function AnalyzeProgress() {
  const { t } = useTranslation();
  const { progress } = useStore();
  const matched = ANALYZE_STEPS.findIndex((stp) => stp.stages.includes(progress.stage));
  // монотонно: неизвестная/незамапленная стадия (напр. промежуточный лог) НЕ гасит прогресс — держим
  // самый дальний достигнутый шаг. Компонент перемонтируется на каждый прогон, поэтому ref сбрасывается.
  const maxStep = useRef(-1);
  if (matched > maxStep.current) maxStep.current = matched;
  const cur = maxStep.current;
  const dl = progress.stage === "download";
  const pct = progress.pct;
  return (
    <div className="flex-1 grid place-items-center px-6">
      <div className="w-full max-w-sm">
        <div className="text-center text-lg font-semibold">{t("analyze.title")}</div>
        {dl && <div className="mt-1 text-center text-[12px] text-[var(--color-muted)]">{t("analyze.firstRunNote")}</div>}
        <div className="mt-6 space-y-2.5">
          {ANALYZE_STEPS.map((stp, i) => {
            const done = cur > i, active = cur === i;
            return (
              <div key={stp.key} className="flex items-center gap-3">
                <span className={`grid place-items-center w-5 h-5 shrink-0 rounded-full ${done ? "bg-[var(--color-accent)] text-[var(--color-on-accent)]" : active ? "text-[var(--color-accent)]" : "text-[var(--color-muted)]/40"}`}>
                  {done ? <Check size={12} /> : active ? <Loader2 size={14} className="animate-spin" /> : <span className="w-1.5 h-1.5 rounded-full bg-current" />}
                </span>
                <span className={`text-sm ${active ? "text-[var(--color-text)] font-medium" : done ? "text-[var(--color-muted)]" : "text-[var(--color-muted)]/45"}`}>{t(`analyze.${stp.key}`)}</span>
                {active && dl && pct != null && <span className="ml-auto mono text-[11px] text-[var(--color-accent)]">{Math.round(pct)}%</span>}
              </div>
            );
          })}
        </div>
        <div className="mt-5 h-1.5 w-full rounded-full bg-[var(--color-surface-2)] overflow-hidden">
          {dl && pct != null
            ? <div className="h-full rounded-full bg-[var(--color-accent)] transition-[width] duration-300" style={{ width: `${Math.max(2, Math.min(100, pct))}%` }} />
            : <div className="h-full w-1/3 rounded-full bg-[var(--color-accent)] animate-pulse" />}
        </div>
        <div className="mt-2 min-h-4 text-center mono text-[12px] text-[var(--color-muted)] break-words">{stageLabel(progress.stage, t) || progress.msg}</div>
      </div>
    </div>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-2 text-[11px] uppercase tracking-[0.16em] text-[var(--color-muted)] mb-3">
      <span className="w-1 h-1 rounded-full bg-[var(--color-accent)]" />{children}
    </div>
  );
}
function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return <div className="flex items-center justify-between"><span className="text-[var(--color-muted)]">{label}</span><span>{children}</span></div>;
}
function Toggle({ label, on, onClick }: { label: string; on: boolean; onClick: () => void }) {
  return (
    <button onClick={onClick} className="flex items-center justify-between w-full">
      <span className="text-[var(--color-muted)]">{label}</span>
      <span className={`w-9 h-5 rounded-full p-0.5 transition-colors ${on ? "bg-[var(--color-accent)]" : "bg-[var(--color-surface-2)]"}`}>
        <span className={`block w-4 h-4 rounded-full bg-white transition-transform ${on ? "translate-x-4" : ""}`} />
      </span>
    </button>
  );
}

function ComparePane({ label, src }: { label: string; src: string }) {
  return (
    <div className="relative h-full min-h-0 grid place-items-center bg-black/40 rounded-xl overflow-hidden">
      <img src={src} alt={label} className="max-h-full max-w-full object-contain" />
      <span className="absolute top-2 left-2 mono text-[10px] px-2 py-0.5 rounded bg-black/60 text-[var(--color-text)] border border-[var(--color-border)]">{label}</span>
    </div>
  );
}

function WaveformTimeline({ pid, duration, scrub, segments, onSeek, gainDb = 0 }: {
  pid: string; duration: number; scrub: number; segments: Project["segments"]; onSeek: (t: number) => void; gainDb?: number;
}) {
  const [peaks, setPeaks] = useState<number[]>([]);
  const wrap = useRef<HTMLDivElement>(null);
  const [w, setW] = useState(800);
  useEffect(() => { api.waveform(pid).then((r) => setPeaks(r.peaks)).catch(() => {}); }, [pid]);
  useEffect(() => { const el = wrap.current; if (!el) return; const ro = new ResizeObserver(() => setW(el.clientWidth)); ro.observe(el); return () => ro.disconnect(); }, []);
  const h = 40, dur = duration || 1, bw = peaks.length ? w / peaks.length : 1;
  const gainLin = Math.pow(10, gainDb / 20);   // dB -> линейный коэффициент амплитуды (гейн дорожки визуально)
  return (
    <div ref={wrap} className="relative w-full overflow-hidden cursor-pointer select-none" style={{ height: h }}
      onClick={(e) => { const r = e.currentTarget.getBoundingClientRect(); onSeek(Math.max(0, Math.min(dur, (e.clientX - r.left) / r.width * dur))); }}>
      <svg width="100%" height={h} viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="block">
        {peaks.map((pk, i) => {
          const bh = Math.max(2, Math.min(h - 2, pk * gainLin * (h - 6))), played = (i / peaks.length) * dur <= scrub;
          return <rect key={i} x={(i / peaks.length) * w} y={(h - bh) / 2} width={Math.max(1, bw - 0.5)} height={bh}
                       fill={played ? "var(--color-accent)" : "#3a414c"} opacity={played ? 0.9 : 0.55} />;
        })}
        {segments.map((s, i) => <rect key={"s" + i} x={(s.start / dur) * w} y={0} width={1} height={h} fill="var(--color-muted)" opacity={0.3} />)}
      </svg>
      <div className="absolute top-0 bottom-0 w-px bg-[var(--color-accent)] shadow-[0_0_6px_var(--color-accent)] pointer-events-none" style={{ left: `${(scrub / dur) * 100}%` }} />
    </div>
  );
}

function CommandPalette({ commands }: { commands: { label: string; run: () => void }[] }) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); setOpen((o) => !o); setQ(""); }
      else if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("keydown", h); return () => window.removeEventListener("keydown", h);
  }, []);
  if (!open) return null;
  const filtered = commands.filter((c) => c.label.toLowerCase().includes(q.toLowerCase()));
  return (
    <div className="fixed inset-0 z-50 grid place-items-start justify-center pt-[14vh] glass-scrim anim-fade" onClick={() => setOpen(false)}>
      <div className="w-[min(92vw,520px)] rounded-xl glass-panel anim-pop overflow-hidden" onClick={(e) => e.stopPropagation()}>
        <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="⌘K  —  команды…"
          className="w-full bg-transparent px-4 py-3 text-[15px] border-b border-[var(--color-border)] focus:outline-none" />
        <div className="max-h-[50vh] overflow-y-auto p-1.5">
          {filtered.map((c, i) => (
            <button key={i} onClick={() => { c.run(); setOpen(false); }}
              className="w-full text-left px-3 py-2 rounded-lg text-[14px] text-[var(--color-text)] hover:bg-[var(--color-surface-2)] transition-colors">{c.label}</button>
          ))}
          {!filtered.length && <div className="px-3 py-4 text-center text-[var(--color-muted)] text-sm">—</div>}
        </div>
      </div>
    </div>
  );
}

function Editor() {
  const { t } = useTranslation();
  const p = useStore((s) => s.project) as Project;
  const rev = useStore((s) => s.rev);   // for the compare 'result' pane refetch (PreviewCanvas reads rev itself)        // subscribe ONLY to what we render -> no re-render on
  const pid = useStore((s) => s.pid) as string;           // rev bumps or progress SSE ticks (those go to PreviewCanvas)
  const rendered = useStore((s) => s.rendered);
  const setProject = useStore((s) => s.setProject);
  const setRendered = useStore((s) => s.setRendered);
  const bump = useStore((s) => s.bump);
  const selBlur = useStore((s) => s.selBlur);             // selected blur zone (shared with the canvas overlay)
  const setSelBlur = useStore((s) => s.setSelBlur);
  const selTitle = useStore((s) => s.selTitle);           // selected title (shared with the canvas overlay)
  const setSelTitle = useStore((s) => s.setSelTitle);
  const pushActivity = useStore((s) => s.pushActivity);   // журнал действий (статус-строка в шапке)
  const rendering = useStore((s) => s.rendering);
  const setRendering = useStore((s) => s.setRendering);
  const addExport = useStore((s) => s.addExport);
  const updateExport = useStore((s) => s.updateExport);
  const pushHistory = useStore((s) => s.pushHistory);
  const undo = useStore((s) => s.undo);
  const redo = useStore((s) => s.redo);
  const canUndo = useStore((s) => s.past.length > 0);
  const canRedo = useStore((s) => s.future.length > 0);
  const [scrub, setScrub] = useState(1.0);
  const [selSegs, setSelSegs] = useState<Set<string>>(new Set());     // multi-select for bulk hide/delete
  const [selBlurs, setSelBlurs] = useState<Set<number>>(new Set());   // multi-select: mask boxes
  const [selTitles, setSelTitles] = useState<Set<number>>(new Set()); // multi-select: titles
  const [fonts, setFonts] = useState<Record<string, string>>({});
  const [voiceList, setVoiceList] = useState<string[]>([]);
  const [spkVoiceBusy, setSpkVoiceBusy] = useState<string | null>(null);
  const [voicePreview, setVoicePreview] = useState<string | null>(null);   // имя проигрываемого сэмпла голоса
  const voicePreviewAudio = useRef<HTMLAudioElement | null>(null);
  // Прослушать/остановить сэмпл выбранного голоса (плей-пауза у дропдауна).
  const toggleVoicePreview = (name: string) => {
    if (!name) return;
    const cur = voicePreviewAudio.current;
    if (voicePreview === name) { cur?.pause(); setVoicePreview(null); return; }   // повторный клик = пауза
    cur?.pause();
    const a = new Audio(api.voiceSampleUrl(name));
    voicePreviewAudio.current = a;
    a.onended = () => setVoicePreview(null);
    a.play().then(() => setVoicePreview(name)).catch(() => setVoicePreview(null));
  };
  const [gainDraft, setGainDraft] = useState<number | null>(null);
  const [voGainDraft, setVoGainDraft] = useState<number | null>(null);   // черновик громкости оригинала (voiceover)
  const [presets, setPresets] = useState<Record<string, Record<string, unknown>>>({});
  useEffect(() => { api.fonts().then((r) => setFonts(r.fonts)).catch(() => {}); }, []);   // bundled caption fonts
  // голоса из каталога; если пусто и ещё не пробовали — тихо тянем дефолтный пак (VibeVoice, ~100МБ) в фоне
  useEffect(() => {
    api.voices().then(async (r) => {
      setVoiceList(r.voices);
      if (r.voices.length === 0 && !localStorage.getItem("voicepack-auto")) {
        localStorage.setItem("voicepack-auto", "1");
        try {
          const { job_id } = await api.voicesDownloadPack();
          await api.watchJob(job_id, () => {});
          const v = await api.voices(); setVoiceList(v.voices);
        } catch { /* тихо: пак опционален */ }
      }
    }).catch(() => {});
  }, []);
  useEffect(() => { api.presets().then((r) => setPresets(r.presets)).catch(() => {}); }, []);   // caption look presets
  const [sizeDraft, setSizeDraft] = useState<number | null>(null);   // live size while dragging (commit on release)
  const [lane, setLane] = useState<"subs" | "blur" | "titles">("subs"); // left lane: which object type to edit
  const [blurAll, setBlurAll] = useState(false);                      // blur: only active-on-frame vs all zones
  const [compare, setCompare] = useState(false);                      // before/after split preview (Topaz-style)
  const [play, setPlay] = useState(false);                            // dub playback: play TTS audio + advance preview frames + playhead
  const audioRef = useRef<HTMLAudioElement>(null);
  const [vol, setVol] = useState<number>(() => { const s = localStorage.getItem("dub-vol"); return s ? parseFloat(s) : 1; });
  const playEndRef = useRef<number>(Infinity);                        // stop time for single-phrase playback (Infinity = full)
  const [dubRev, setDubRev] = useState(0);                            // dub-audio cache-buster — bumped ONLY when the dub track is re-rendered (regen/export), NOT on every edit, so live edits don't reload <audio> mid-playback
  useEffect(() => { if (audioRef.current) audioRef.current.volume = vol; }, [vol, dubRev]);   // применить громкость прослушки (и при перезагрузке трека)
  // Dub playback — drive the EDITOR preview from the dub audio track (no video element): the preview frame and the
  // waveform playhead follow audio.currentTime, so you hear the dub while the future result plays frame-by-frame.
  useEffect(() => {
    const a = audioRef.current; if (!a) return;
    if (!play) { a.pause(); return; }
    setRendered(false); a.play().catch(() => {});
    const id = window.setInterval(() => {
      if (a.currentTime >= playEndRef.current) { a.pause(); setPlay(false); }   // single phrase -> stop at its end
      else setScrub(a.currentTime);
    }, 150);
    return () => window.clearInterval(id);
  }, [play]);   // eslint-disable-line react-hooks/exhaustive-deps
  const [regenId, setRegenId] = useState<string | null>(null);        // segment whose TTS is being re-generated
  const [remixText, setRemixText] = useState("");                     // funny-remix theme/instruction for Gemma
  const [remixing, setRemixing] = useState(false);
  const ss = p.captions.sub_style;
  // уникальные спикеры проекта (для переброса фразы другому спикеру на плашке; голос спикера — в настройках голосов)
  const speakers = Array.from(new Set(p.segments.map((s) => s.speaker).filter((s): s is string => s != null && s !== "")))
    .sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
  // one undo snapshot per edit BURST (focus->type->blur), segments AND titles: snapshot on the FIRST change of a
  // field, keyed by field, cleared on blur. Not on focus (that killed redo) nor per-keystroke (that flooded history).
  const burstRef = useRef<string | null>(null);

  function patchSeg(id: string, tgt: string) {                       // instant local echo while typing
    if (burstRef.current !== `seg:${id}`) { pushHistory(p); burstRef.current = `seg:${id}`; }
    setProject({ ...p, segments: p.segments.map((x) => x.id === id ? { ...x, tgt_text: tgt, dirty: true } : x) });
  }
  function titleText(i: number, text: string) {                      // instant local echo. РЕНДЕР берёт tgt (если непустой), поэтому правим И tgt И text — иначе правка текста не видна на кадре
    if (burstRef.current !== `title:${i}`) { pushHistory(p); burstRef.current = `title:${i}`; }
    setProject({ ...p, captions: { ...p.captions, titles: p.captions.titles.map((x, j) => j === i ? { ...x, text, tgt: text } : x) } });
  }
  // a patch/PUT rejected (4xx/5xx/offline) -> surface it in the Files panel (like doExport) and re-sync from
  // the server so the optimistic local echo can't silently diverge from persisted truth
  async function surfaceErr(err: unknown) {
    pushActivity(String(err), "error"); playSfx("error");
    addExport({ id: `err-${Date.now()}`, name: t("common.error"), status: "error", msg: String(err) });
    try { setProject(await api.getProject(pid)); } catch { /* offline -> keep optimistic state */ }
  }
  async function persistSeg(id: string, tgt: string) {               // on blur -> persist to backend + refresh frame
    setRendered(false);
    try { setProject(await api.patch(pid, { op: "segment", id, tgt_text: tgt })); bump(); }
    catch (err) { await surfaceErr(err); }
  }
  async function branch(op: string, extra: Record<string, unknown> = {}) {
    pushHistory(p);                                                  // snapshot for undo BEFORE the mutation
    setRendered(false);
    try { const fresh = await api.patch(pid, { op, ...extra }); setProject(fresh); bump(); return fresh; }   // style/voice/text change -> re-fetch the frame
    catch (err) { await surfaceErr(err); return null; }
  }
  async function doRegen(segId: string) {                            // re-synthesize the TTS for ONE phrase (mark dirty -> /render)
    if (regenId) return;
    setRegenId(segId); pushActivity(t("seg.regen"));
    try {
      await api.patch(pid, { op: "regen", id: segId });
      const { job_id } = await api.render(pid);                       // re-TTS only the dirty seg + re-mux -> fresh dub
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); setRendered(false); bump(); setDubRev(Date.now()); playSfx("notify");   // refresh preview + reload the re-rendered dub audio
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function doHideSeg(segId: string) {                          // toggle a line off/on — drops its subtitle AND its dub audio
    if (regenId) return;
    pushHistory(p); setRegenId(segId); setRendered(false);
    try {
      await api.patch(pid, { op: "hide_segment", id: segId });
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); bump(); setDubRev(Date.now());
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function doDelSeg(segId: string) {                           // delete a line entirely — subtitle + dub audio gone (undoable)
    if (regenId) return;
    pushHistory(p); setRegenId(segId); setRendered(false);
    try {
      await api.patch(pid, { op: "del_segment", id: segId });
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); bump(); setDubRev(Date.now());
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function doKeepSeg(segId: string) {                          // toggle 'keep original audio' — source plays, no dub, no sub
    if (regenId) return;
    pushHistory(p); setRegenId(segId); setRendered(false);
    try {
      await api.patch(pid, { op: "keep_segment", id: segId });
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); bump(); setDubRev(Date.now());
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function bulkDelIdx(op: "del_titles" | "del_blurs", idxs: Set<number>, clear: () => void) {
    if (!idxs.size) return;                                           // bulk-delete several titles / mask boxes by index
    pushHistory(p); setRendered(false);
    try {
      setProject(await api.patch(pid, { op, idxs: [...idxs] })); bump(); clear();
      if (op === "del_blurs") setSelBlur(null); else setSelTitle(null);   // индексы съехали -> сбросить одиночный выбор
    }
    catch (e) { await surfaceErr(e); }
  }
  async function bulkSeg(op: "del_segments" | "hide_segments" | "keep_segments", extra: Record<string, unknown> = {}) {
    if (!selSegs.size || regenId) return;                            // bulk hide/delete the selected lines, ONE re-render
    pushHistory(p); setRegenId("__bulk__"); setRendered(false);
    try {
      await api.patch(pid, { op, ids: [...selSegs], ...extra });
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); bump(); setDubRev(Date.now()); setSelSegs(new Set());
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function doGain(gainDb: number) {                            // монтажный гейн всей дорожки: патч + лёгкий ре-рендер (ре-TTS не нужен, сегменты из кэша)
    if (regenId) return;
    setRegenId("__all__"); pushActivity(t("voice.gain"));
    try {
      await api.patch(pid, { op: "gain", gain_db: gainDb });
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); setRendered(false); setDubRev(Date.now());   // покадровое превью; /dub обновлён -> плей играет новый дуб
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function doVoiceoverGain(gainDb: number) {                   // громкость оригинала под переводом (voiceover): патч + пересведение (ре-TTS не нужен)
    if (regenId) return;
    setRegenId("__all__"); pushActivity(t("voice.origGain"));
    try {
      await api.patch(pid, { op: "voiceover_gain", gain_db: gainDb });
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); setRendered(false); setDubRev(Date.now());   // покадровое превью; /dub обновлён -> плей играет закадр с новым балансом
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function doRegenAll() {                                      // re-synthesize the WHOLE dub (after switching the pack voice/speaker, or to re-roll)
    if (regenId) return;
    setRegenId("__all__"); pushActivity(t("voice.regenAll"));       // sentinel: disables per-seg regen buttons, no per-seg spinner
    try {
      await api.patch(pid, { op: "regen_all" });                    // mark every segment dirty
      const { job_id } = await api.render(pid);                     // re-TTS all dirty segs + re-mux -> fresh dub
      await api.watchJob(job_id, () => {});
      setProject(await api.getProject(pid)); setRendered(false); bump(); setDubRev(Date.now()); playSfx("notify");   // покадровое превью; /dub обновлён -> плей играет новый дуб
    } catch (e) { await surfaceErr(e); }
    finally { setRegenId(null); }
  }
  async function forceSeg(seg: Project["segments"][number]) {        // force-refresh ONE phrase: re-seek + re-fetch + re-render (if stuck)
    setScrub(seg.start); setRendered(false);
    setProject(await api.getProject(pid)); bump();
  }
  function playFull() {                                               // bottom-bar Play: play the whole dub from the playhead
    const a = audioRef.current;
    if (play) { setPlay(false); return; }
    playEndRef.current = Infinity; if (a) a.currentTime = scrub; setPlay(true);
  }
  function playSeg(seg: Project["segments"][number]) {               // play JUST this phrase's TTS [start, end]
    const a = audioRef.current; if (!a) return;
    playEndRef.current = seg.end; a.currentTime = seg.start; setScrub(seg.start); setRendered(false);
    if (play) a.play().catch(() => {}); else setPlay(true);
  }
  async function doUndo() { const prev = undo(); if (prev) { setSelBlur(null); setSelTitle(null); setRendered(false); await api.putProject(pid, prev); bump(); } }
  async function doRedo() { const next = redo(); if (next) { setSelBlur(null); setSelTitle(null); setRendered(false); await api.putProject(pid, next); bump(); } }
  useEffect(() => {                                                  // Cmd/Ctrl+Z / Shift+Z / Y (not while typing in a field)
    const h = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      const k = e.key.toLowerCase();
      if (k === "z" && !e.shiftKey) { e.preventDefault(); doUndo(); }
      else if ((k === "z" && e.shiftKey) || k === "y") { e.preventDefault(); doRedo(); }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, [pid]);   // eslint-disable-line react-hooks/exhaustive-deps
  async function doExport() {
    const exId = `export-${pid}`;   // одна запись на проект (повторный экспорт заменяет её, а не плодит дубли)
    const name = (p.meta.video || pid).split(/[\\/]/).pop() || pid;
    addExport({ id: exId, name, status: "rendering", msg: t("common.rendering"), pid });   // queue entry -> Files panel (no screen block)
    setRendering(true); pushActivity(`${t("export.proceed")}: ${name}`);
    try {
      const { job_id } = await api.render(pid);
      await api.watchJob(job_id, (e) => { if (e.type === "progress") { updateExport(exId, { msg: e.msg || "" }); pushActivity(e.msg || "", "work"); } });
      updateExport(exId, { status: "done", msg: "", url: `${api.outputUrl(pid)}?rev=${Date.now()}` });   // bust cache on re-export
      pushActivity(`${t("compare.result")}: ${name}`, "done"); playSfx("success");
      setRendered(true); setDubRev(Date.now());   // /dub now serves the freshly rendered output.mp4 -> reload <audio>
    } catch (err) {
      updateExport(exId, { status: "error", msg: String(err) }); pushActivity(String(err), "error"); playSfx("error");
    } finally { setRendering(false); }
  }
  async function doRemix() {                                            // Gemma rewrites the WHOLE script on a theme
    if (!remixText.trim() || remixing) return;
    setRemixing(true); pushActivity(t("remix.apply"));
    try {
      pushHistory(p);
      const { job_id } = await api.remix(pid, remixText.trim());
      await api.watchJob(job_id, (e) => { if (e.type === "progress") useStore.getState().setProgress(e.stage || "remix", e.msg || t("remix.apply"), e.pct ?? null); });
      setRendered(false);
      setProject(await api.getProject(pid));                            // rewritten transcript -> shows in the lane
      bump(); setLane("subs");                                          // показать переписанный текст сразу
      useStore.getState().setProgress("done", t("remix.apply"), null); // done-строка в журнале
    } catch (err) { await surfaceErr(err); }                           // было: тихий console.error -> провал не был виден
    finally { setRemixing(false); }
  }

  const isActive = (seg: Project["segments"][number]) => scrub >= seg.start && scrub < seg.end;
  const activeId = p.segments.find(isActive)?.id;
  const mode = p.mode === "voiceover" ? "voiceover" : p.mode === "transcribe" ? "transcribe" : p.audio.rewrite ? "funny" : (p.mode === "nodub" ? "subtitles" : "dub");   // derived output mode
  const MODES = [["subtitles", Captions], ["dub", AudioLines], ["voiceover", Mic2], ["funny", Sparkles], ["transcribe", FileText]] as const;
  const cmds = [
    { label: t("mode.subtitles"), run: () => branch("mode", { value: "subtitles" }) },
    { label: t("mode.dub"), run: () => branch("mode", { value: "dub" }) },
    { label: t("mode.voiceover"), run: () => branch("mode", { value: "voiceover" }) },
    { label: t("mode.funny"), run: () => branch("mode", { value: "funny" }) },
    { label: t("mode.transcribe"), run: () => branch("mode", { value: "transcribe" }) },
    { label: t("export.proceed"), run: () => doExport() },
    { label: t("common.undo"), run: () => doUndo() },
    { label: t("common.redo"), run: () => doRedo() },
    { label: t("compare.toggle"), run: () => setCompare((c) => !c) },
    ...Object.keys(presets).map((n) => ({ label: `${t("preset.title")}: ${n}`, run: () => branch("preset", { name: n }) })),
  ];
  const activeRef = useRef<HTMLDivElement>(null);
  useEffect(() => { activeRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" }); }, [activeId]);
  return (
    <div className="flex-1 grid grid-cols-[330px_1fr_300px] min-h-0">
      <aside className="border-r border-[var(--color-border)] overflow-y-auto p-4 bg-[var(--color-surface)]">
        <div className="inline-flex rounded-lg bg-[var(--color-surface-2)] p-0.5 border border-[var(--color-border)] mb-3 text-[12px]">
          {([["subs", t("mode.subtitles")], ["blur", `${t("blur.title")} ${(p.captions.blur_boxes || []).length}`],
            ["titles", `${t("titles.tab")} ${(p.captions.titles || []).length}`]] as const).map(([k, lbl]) => (
            <button key={k} onClick={() => setLane(k as typeof lane)}
              className={`px-2.5 py-1 rounded-md transition-colors ${lane === k ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] font-semibold" : "text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
              {lbl}
            </button>
          ))}
        </div>
        {lane === "subs" && (
        <div className="space-y-2">
          {(() => {
            const spks = [...new Set(p.segments.map((x) => x.speaker ?? "0"))].sort();
            const idsOf = (spk: string | null) => p.segments.filter((x) => spk === null || (x.speaker ?? "0") === spk).map((x) => x.id);
            const toggleMany = (ids: string[]) => setSelSegs((prev) => {
              const next = new Set(prev), all = ids.length > 0 && ids.every((i) => next.has(i));
              ids.forEach((i) => (all ? next.delete(i) : next.add(i)));
              return next;
            });
            const chip = "px-2 py-0.5 rounded-md text-[11px] border border-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors";
            return (
              <div className="space-y-1.5 mb-1">
                <div className="flex flex-wrap items-center gap-1">
                  <span className="text-[10px] uppercase tracking-wider text-[var(--color-muted)] mr-0.5">{t("sel.pick")}</span>
                  <button onClick={() => toggleMany(idsOf(null))} className={chip}>{t("sel.all")}</button>
                  {spks.length > 1 && spks.map((spk) => <button key={spk} onClick={() => toggleMany(idsOf(spk))} className={chip}>SPK {spk}</button>)}
                </div>
                {selSegs.size > 0 && (
                  <div className="flex items-center gap-1.5 rounded-lg border border-[var(--color-accent)]/50 bg-[color-mix(in_oklab,var(--color-accent)_8%,transparent)] px-2 py-1.5">
                    <span className="text-[12px] font-medium">{selSegs.size} {t("sel.count")}</span>
                    <button onClick={() => bulkSeg("keep_segments", { keep: true })} disabled={regenId !== null}
                      className="ml-auto inline-flex items-center gap-1 px-2 py-1 rounded-md bg-[var(--color-surface-2)] text-[12px] hover:text-[var(--color-accent)] disabled:opacity-40 transition-colors"><Music size={13} />{t("sel.keep")}</button>
                    <button onClick={() => bulkSeg("hide_segments", { hidden: true })} disabled={regenId !== null}
                      className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-[var(--color-surface-2)] text-[12px] hover:text-[var(--color-accent)] disabled:opacity-40 transition-colors"><EyeOff size={13} />{t("sel.hide")}</button>
                    <button onClick={() => bulkSeg("del_segments")} disabled={regenId !== null}
                      className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-[var(--color-surface-2)] text-[12px] hover:text-[#ef4444] disabled:opacity-40 transition-colors"><Trash2 size={13} />{t("sel.del")}</button>
                    <button onClick={() => setSelSegs(new Set())} className="text-[var(--color-muted)] hover:text-[var(--color-text)]"><X size={14} /></button>
                  </div>
                )}
              </div>
            );
          })()}
          {p.segments.map((seg) => {
            const on = isActive(seg);
            return (
              <div key={seg.id} ref={on ? activeRef : undefined}
                onClick={() => { setRendered(false); setScrub(seg.start); }}   // click a phrase -> seek the playhead to it
                className={`rounded-xl p-2.5 border-l-2 transition-colors cursor-pointer ${seg.hidden ? "opacity-50" : ""} ${selSegs.has(seg.id) ? "ring-1 ring-[var(--color-accent)]/60" : ""} ${on ? "bg-[var(--color-surface-2)] border-[var(--color-accent)]" : "bg-[var(--color-surface-2)]/40 border-transparent hover:bg-[var(--color-surface-2)]/70"}`}>
                <div className="flex items-center gap-2">
                  <button onClick={(e) => { e.stopPropagation(); setSelSegs((prev) => { const n = new Set(prev); n.has(seg.id) ? n.delete(seg.id) : n.add(seg.id); return n; }); }}
                    className={`grid place-items-center w-4 h-4 rounded shrink-0 border transition-colors ${selSegs.has(seg.id) ? "bg-[var(--color-accent)] border-[var(--color-accent)] text-[var(--color-on-accent)]" : "border-[var(--color-border)] hover:border-[var(--color-accent)]"}`}>
                    {selSegs.has(seg.id) && <Check size={11} />}</button>
                  <span className={`mono text-[11px] px-1.5 py-0.5 rounded tabnum ${on ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] font-semibold" : "bg-[var(--color-overlay)] text-[var(--color-muted)]"}`}>{fmtT(seg.start)}</span>
                  <span className="mono text-[10px] text-[var(--color-muted)]/60 tabnum">→ {fmtT(seg.end)}</span>
                  {seg.speaker != null && <span className="mono px-1.5 py-px rounded bg-[var(--color-overlay)] text-[9px] text-[var(--color-muted)]">SPK {seg.speaker}</span>}
                  <span className="ml-auto flex items-center gap-1">
                    {seg.dirty && <span className="text-[var(--color-accent)] text-[10px] mr-0.5" title="edited">●</span>}
                    <button onClick={(e) => { e.stopPropagation(); playSeg(seg); }} title={t("seg.play")}
                      className="text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><Play size={12} /></button>
                    <button onClick={(e) => { e.stopPropagation(); doRegen(seg.id); }} disabled={regenId !== null} title={t("seg.regen")}
                      className="text-[var(--color-muted)] hover:text-[var(--color-accent)] disabled:opacity-40 transition-colors">
                      {regenId === seg.id ? <Loader2 size={12} className="animate-spin" /> : <RotateCw size={12} />}
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); forceSeg(seg); }} title={t("seg.refreshHint")}
                      className="text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><RefreshCw size={12} /></button>
                    <button onClick={(e) => { e.stopPropagation(); doKeepSeg(seg.id); }} disabled={regenId !== null} title={seg.keep_original ? t("seg.unkeep") : t("seg.keep")}
                      className={`disabled:opacity-40 transition-colors ${seg.keep_original ? "text-[var(--color-accent)]" : "text-[var(--color-muted)] hover:text-[var(--color-accent)]"}`}><Music size={12} /></button>
                    <button onClick={(e) => { e.stopPropagation(); doHideSeg(seg.id); }} disabled={regenId !== null} title={seg.hidden ? t("seg.show") : t("seg.hide")}
                      className="text-[var(--color-muted)] hover:text-[var(--color-accent)] disabled:opacity-40 transition-colors">
                      {seg.hidden ? <EyeOff size={12} /> : <Eye size={12} />}</button>
                    <button onClick={(e) => { e.stopPropagation(); doDelSeg(seg.id); }} disabled={regenId !== null} title={t("seg.del")}
                      className="text-[var(--color-muted)] hover:text-[#ef4444] disabled:opacity-40 transition-colors"><Trash2 size={12} /></button>
                  </span>
                </div>
                <div className="text-[11px] text-[var(--color-muted)]/80 mt-1.5 leading-snug">{seg.src_text}</div>
                <textarea value={seg.tgt_text} onChange={(e) => patchSeg(seg.id, e.target.value)}
                  onClick={(e) => e.stopPropagation()}                       // editing text must not re-seek on every click
                  onBlur={(e) => { burstRef.current = null; persistSeg(seg.id, e.target.value); }}   // end the edit burst
                  className="w-full mt-1.5 bg-[var(--color-bg)]/60 border border-[var(--color-border)] rounded-lg p-1.5 text-[13px] leading-snug resize-none focus:border-[var(--color-accent)] focus:outline-none transition-colors" rows={2} />
                {speakers.length > 1 && !seg.keep_original && (
                  <div className="flex items-center gap-1.5 mt-1.5" onClick={(e) => e.stopPropagation()} title={t("seg.speakerHint")}>
                    <Users size={11} className="text-[var(--color-muted)] shrink-0" />
                    <select value={seg.speaker ?? ""}
                      onChange={async (e) => { setRendered(false); try { setProject(await api.patch(pid, { op: "segment", id: seg.id, speaker: e.target.value })); bump(); } catch (err) { await surfaceErr(err); } }}
                      className="flex-1 min-w-0 bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1.5 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none transition-colors">
                      {seg.speaker == null && <option value="">—</option>}
                      {speakers.map((s) => <option key={s} value={s}>SPK {s}</option>)}
                    </select>
                  </div>
                )}
              </div>
            );
          })}
        </div>
        )}
        {lane === "blur" && (
          <div className="space-y-2">
            <Toggle label={t("blur.on")} on={p.render.blur} onClick={() => branch("blur_enable", { on: !p.render.blur })} />
            <div className={p.render.blur ? "" : "opacity-40 pointer-events-none"}>
              <div className="flex items-center justify-between mt-2 mb-1.5">
                <span className="mono text-[10px] text-[var(--color-muted)]">{blurAll ? `${t("blur.all")} · ${(p.captions.blur_boxes || []).length}` : t("blur.frame")}</span>
                <button onClick={() => setBlurAll(!blurAll)} className="mono text-[10px] text-[var(--color-accent)] hover:underline">
                  {blurAll ? t("blur.frame") : `${t("blur.all")} (${(p.captions.blur_boxes || []).length})`}
                </button>
              </div>
              {(p.captions.blur_boxes || []).length > 0 && (
                <div className="flex items-center gap-1.5 mb-1.5">
                  <button onClick={() => setSelBlurs((prev) => prev.size === (p.captions.blur_boxes || []).length ? new Set() : new Set((p.captions.blur_boxes || []).map((_, i) => i)))}
                    className="px-2 py-0.5 rounded-md text-[11px] border border-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors">{t("sel.all")}</button>
                  {selBlurs.size > 0 && (<>
                    <span className="text-[11px] text-[var(--color-muted)]">{selBlurs.size} {t("sel.count")}</span>
                    <button onClick={() => bulkDelIdx("del_blurs", selBlurs, () => setSelBlurs(new Set()))}
                      className="ml-auto inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[11px] bg-[var(--color-surface-2)] hover:text-[#ef4444] transition-colors"><Trash2 size={12} />{t("sel.del")}</button>
                  </>)}
                </div>
              )}
              <div className="space-y-1 max-h-[46vh] overflow-y-auto pr-1">
                {(p.captions.blur_boxes || []).map((b, i) => ({ b, i }))
                  .filter(({ b }) => blurAll || (scrub >= b.t0 - 0.6 && scrub <= b.t1 + 0.4))
                  .map(({ b, i }) => (
                    <div key={i} onClick={() => { setSelBlur(i); setRendered(false); setScrub(Math.max(b.t0, 0)); }}
                      className={`flex items-center gap-2 mono text-[10px] rounded px-2 py-1 cursor-pointer transition-colors ${selBlur === i ? "bg-[color-mix(in_oklab,var(--color-accent)_18%,transparent)] text-[var(--color-text)] ring-1 ring-[var(--color-accent)]" : "text-[var(--color-muted)] bg-[var(--color-surface-2)]/40 hover:text-[var(--color-text)]"} ${b.hidden ? "opacity-50" : ""} ${selBlurs.has(i) ? "ring-1 ring-[var(--color-accent)]" : ""}`}>
                      <button onClick={(e) => { e.stopPropagation(); setSelBlurs((prev) => { const n = new Set(prev); n.has(i) ? n.delete(i) : n.add(i); return n; }); }}
                        className={`grid place-items-center w-3.5 h-3.5 rounded-sm shrink-0 border transition-colors ${selBlurs.has(i) ? "bg-[var(--color-accent)] border-[var(--color-accent)] text-[var(--color-on-accent)]" : "border-[var(--color-border)] hover:border-[var(--color-accent)]"}`}>{selBlurs.has(i) && <Check size={9} />}</button>
                      <button onClick={(e) => { e.stopPropagation(); branch("blur", { idx: i, hidden: !b.hidden }); }}
                        title={b.hidden ? t("blur.show") : t("blur.hide")}
                        className="shrink-0 hover:text-[var(--color-accent)] transition-colors">{b.hidden ? <EyeOff size={12} /> : <Eye size={12} />}</button>
                      <span className="flex-1 truncate">#{i + 1} · {b.w}×{b.h} · {fmtT(b.t0)}{b.hidden ? ` · ${t("blur.off")}` : ""}</span>
                      {b.fill && (
                        <input type="color" value={b.fill} onClick={(e) => e.stopPropagation()}
                          onChange={(e) => { e.stopPropagation(); branch("blur", { idx: i, fill: e.target.value }); }}
                          title={t("blur.fillColor")} className="w-4 h-4 shrink-0 p-0 border-0 bg-transparent rounded cursor-pointer" />
                      )}
                      <button onClick={(e) => { e.stopPropagation(); branch("blur", { idx: i, fill: b.fill ? null : "#000000" }); }}
                        title={b.fill ? t("blur.modeFill") : t("blur.modeBlur")}
                        className="shrink-0 hover:text-[var(--color-accent)] transition-colors">{b.fill ? <Square size={12} /> : <Droplet size={12} />}</button>
                      <span className="inline-flex rounded border border-[var(--color-border)] overflow-hidden shrink-0">
                        <button onClick={(e) => { e.stopPropagation(); branch("blur", { idx: i, t0: Math.max(scrub, 0) }); }} title={t("edit.setStart")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ChevronFirst size={13} /></button>
                        <button onClick={(e) => { e.stopPropagation(); branch("blur", { idx: i, t1: scrub }); }} title={t("edit.setEnd")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ChevronLast size={13} /></button>
                        <button onClick={(e) => { e.stopPropagation(); branch("blur", { idx: i, t0: 0 }); }} title={t("edit.startVideo")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ArrowLeftToLine size={13} /></button>
                        <button onClick={(e) => { e.stopPropagation(); branch("blur", { idx: i, t1: p.meta.duration || 0 }); }} title={t("edit.endVideo")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ArrowRightToLine size={13} /></button>
                      </span>
                      <button onClick={(e) => { e.stopPropagation(); branch("blur_del", { idx: i }); setSelBlur(null); }} className="shrink-0 hover:text-[var(--color-warn)] transition-colors"><Trash2 size={12} /></button>
                    </div>
                  ))}
                {!(p.captions.blur_boxes || []).some((b) => blurAll || (scrub >= b.t0 - 0.6 && scrub <= b.t1 + 0.4)) &&
                  <div className="text-[11px] text-[var(--color-muted)]/50 py-3 text-center">—</div>}
              </div>
              <button onClick={async () => { const fresh = await branch("blur_add", { x: Math.round((p.meta.width || 0) * 0.25), y: Math.round((p.meta.height || 0) * 0.45), w: Math.round((p.meta.width || 0) * 0.5), h: Math.round((p.meta.height || 0) * 0.08), t0: Math.max(0, scrub - 1), t1: scrub + 2 }); if (fresh) setSelBlur((fresh.captions.blur_boxes || []).length - 1); }}
                className="w-full mt-1.5 inline-flex items-center justify-center gap-1.5 text-[12px] py-1.5 rounded-lg border border-dashed border-[var(--color-border)] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)] transition-colors">
                <Plus size={13} /> {t("blur.add")}
              </button>
            </div>
          </div>
        )}
        {lane === "titles" && (
          <div className="space-y-2">
            {!(p.captions.titles || []).length && <div className="text-[11px] text-[var(--color-muted)]/50 py-3 text-center">—</div>}
            {(p.captions.titles || []).length > 0 && (
              <div className="flex items-center gap-1.5 mb-1">
                <button onClick={() => setSelTitles((prev) => prev.size === (p.captions.titles || []).length ? new Set() : new Set((p.captions.titles || []).map((_, i) => i)))}
                  className="px-2 py-0.5 rounded-md text-[11px] border border-[var(--color-border)] bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)] hover:border-[var(--color-accent)] transition-colors">{t("sel.all")}</button>
                {selTitles.size > 0 && (<>
                  <span className="text-[11px] text-[var(--color-muted)]">{selTitles.size} {t("sel.count")}</span>
                  <button onClick={() => bulkDelIdx("del_titles", selTitles, () => setSelTitles(new Set()))}
                    className="ml-auto inline-flex items-center gap-1 px-2 py-0.5 rounded-md text-[11px] bg-[var(--color-surface-2)] hover:text-[#ef4444] transition-colors"><Trash2 size={12} />{t("sel.del")}</button>
                </>)}
              </div>
            )}
            {(p.captions.titles || []).map((ti, i) => (
              <div key={`${ti.start}_${ti.end}_${i}`} onClick={() => { setSelTitle(i); setRendered(false); setScrub(Math.max(ti.start, 0)); }}
                className={`rounded-xl p-2.5 bg-[var(--color-surface-2)]/50 cursor-pointer transition-shadow ${selTitle === i ? "ring-1 ring-[var(--color-accent)]" : ""}`}>
                <div className="flex items-center gap-2 mono text-[10px] text-[var(--color-muted)] mb-1.5">
                  <button onClick={() => setSelTitles((prev) => { const n = new Set(prev); n.has(i) ? n.delete(i) : n.add(i); return n; })}
                    className={`grid place-items-center w-3.5 h-3.5 rounded-sm shrink-0 border transition-colors ${selTitles.has(i) ? "bg-[var(--color-accent)] border-[var(--color-accent)] text-[var(--color-on-accent)]" : "border-[var(--color-border)] hover:border-[var(--color-accent)]"}`}>{selTitles.has(i) && <Check size={9} />}</button>
                  <span className="tabnum">{fmtT(ti.start)} → {fmtT(ti.end)}</span>
                  <button onClick={(e) => { e.stopPropagation(); branch("title_del", { idx: i }); setSelTitle(null); }} className="ml-auto hover:text-[var(--color-warn)] transition-colors" title="delete"><Trash2 size={12} /></button>
                </div>
                <input value={ti.tgt || ti.text} onChange={(e) => titleText(i, e.target.value)} onClick={(e) => e.stopPropagation()}
                  onBlur={async (e) => { burstRef.current = null; setRendered(false); try { setProject(await api.patch(pid, { op: "title", idx: i, text: e.target.value, tgt: e.target.value })); bump(); } catch (err) { await surfaceErr(err); } }}
                  className="w-full bg-[var(--color-bg)]/60 border border-[var(--color-border)] rounded p-1.5 text-[13px] focus:border-[var(--color-accent)] focus:outline-none transition-colors" />
                <div className="flex items-center gap-1.5 mt-1.5 flex-wrap">
                  <button onClick={() => branch("title", { idx: i, bold: !ti.bold })}
                    className={`text-[11px] font-bold px-2 py-0.5 rounded border transition-colors ${ti.bold ? "border-[var(--color-accent)] text-[var(--color-accent)]" : "border-[var(--color-border)] text-[var(--color-muted)]"}`}>{t("style.bold")}</button>
                  <button onClick={() => branch("title", { idx: i, italic: !ti.italic })}
                    className={`text-[11px] italic px-2 py-0.5 rounded border transition-colors ${ti.italic ? "border-[var(--color-accent)] text-[var(--color-accent)]" : "border-[var(--color-border)] text-[var(--color-muted)]"}`}>{t("style.italic")}</button>
                  <button onClick={() => branch("title", { idx: i, uppercase: !ti.uppercase })} title={t("style.caps")}
                    className={`text-[11px] font-semibold px-2 py-0.5 rounded border transition-colors ${ti.uppercase ? "border-[var(--color-accent)] text-[var(--color-accent)]" : "border-[var(--color-border)] text-[var(--color-muted)]"}`}>AA</button>
                  <input type="color" value={ti.color || "#FFFFFF"} onChange={(e) => branch("title", { idx: i, color: e.target.value })}
                    title={t("style.color")} className="w-7 h-6 rounded bg-transparent cursor-pointer border border-[var(--color-border)]" />
                  <input type="color" value={ti.outline || "#000000"} onChange={(e) => branch("title", { idx: i, outline: e.target.value })}
                    title={t("style.outline")} className="w-7 h-6 rounded bg-transparent cursor-pointer border border-dashed border-[var(--color-border)]" />
                  <input key={`ow${i}-${ti.outline_w ?? "a"}`} type="number" min={0} max={20} defaultValue={ti.outline_w ?? undefined} placeholder={t("style.outlineW")} title={t("style.outlineWFull")}
                    onBlur={(e) => branch("title", { idx: i, outline_w: e.target.value === "" ? null : parseInt(e.target.value) })}
                    className="w-12 bg-[var(--color-surface-2)] border border-dashed border-[var(--color-border)] rounded px-1 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none" />
                  <select value={ti.shadow_dir ?? ""} title={t("style.shadow")}
                    onChange={(e) => branch("title", { idx: i, shadow_dir: e.target.value === "" ? null : parseInt(e.target.value) })}
                    className="bg-[var(--color-surface-2)] border border-dashed border-[var(--color-border)] rounded px-1 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none">
                    <option value="">—</option><option value="270">↑</option><option value="315">↗</option><option value="0">→</option><option value="45">↘</option><option value="90">↓</option><option value="135">↙</option><option value="180">←</option><option value="225">↖</option>
                  </select>
                  <input key={`sz${i}-${ti.size_px ?? "a"}`} type="number" min={12} max={300} defaultValue={ti.size_px ?? undefined} placeholder="px" title={t("style.size")}
                    onBlur={(e) => branch("title", { idx: i, size_px: e.target.value ? parseInt(e.target.value) : null })}
                    className="w-12 bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none" />
                  <select value={ti.font || ""} onChange={(e) => branch("title", { idx: i, font: e.target.value })}
                    className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1.5 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none">
                    <option value="">{t("style.font")}</option>
                    {Object.keys(fonts).map((f) => <option key={f} value={f}>{f}</option>)}
                  </select>
                  <span className="inline-flex rounded border border-[var(--color-border)] overflow-hidden">
                    {([["left", AlignLeft, "edit.alignLeft"], ["center", AlignCenter, "edit.alignCenter"], ["right", AlignRight, "edit.alignRight"]] as const).map(([a, Ic, k]) => (
                      <button key={a} onClick={(e) => { e.stopPropagation(); branch("title", { idx: i, align: a }); }} title={t(k)}
                        className={`px-1.5 py-1 transition-colors ${(ti.align || "center") === a ? "bg-[var(--color-accent)] text-[var(--color-on-accent)]" : "text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}><Ic size={12} /></button>
                    ))}
                  </span>
                  <span className="inline-flex rounded border border-[var(--color-border)] overflow-hidden shrink-0">
                    <button onClick={(e) => { e.stopPropagation(); branch("title", { idx: i, start: scrub }); }} title={t("edit.setStart")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ChevronFirst size={13} /></button>
                    <button onClick={(e) => { e.stopPropagation(); branch("title", { idx: i, end: scrub }); }} title={t("edit.setEnd")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ChevronLast size={13} /></button>
                    <button onClick={(e) => { e.stopPropagation(); branch("title", { idx: i, start: 0 }); }} title={t("edit.startVideo")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ArrowLeftToLine size={13} /></button>
                    <button onClick={(e) => { e.stopPropagation(); branch("title", { idx: i, end: p.meta.duration || 0 }); }} title={t("edit.endVideo")} className="px-1.5 py-1 text-[var(--color-muted)] hover:text-[var(--color-accent)] transition-colors"><ArrowRightToLine size={13} /></button>
                  </span>
                </div>
              </div>
            ))}
            <button onClick={async () => { const fresh = await branch("title_add", { text: "Title", x: Math.round((p.meta.width || 0) * 0.15), y: Math.round((p.meta.height || 0) * 0.4), w: Math.round((p.meta.width || 0) * 0.7), h: Math.round((p.meta.height || 0) * 0.1), t0: Math.max(0, scrub - 0.5), t1: scrub + 3 }); if (fresh) setSelTitle((fresh.captions.titles || []).length - 1); }}
              className="w-full inline-flex items-center justify-center gap-1.5 text-[12px] py-1.5 rounded-lg border border-dashed border-[var(--color-border)] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)] transition-colors">
              <Plus size={13} /> {t("titles.add")}
            </button>
          </div>
        )}
      </aside>

      <main className="flex flex-col min-w-0 min-h-0 overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-2.5 border-b border-[var(--color-border)] bg-[var(--color-surface)]">
          <div className="inline-flex rounded-lg bg-[var(--color-surface-2)] p-0.5 border border-[var(--color-border)] shrink-0">
            {MODES.map(([k, Ic]) => (
              <button key={k} onClick={() => branch("mode", { value: k })}
                className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-md text-[13px] transition-colors ${mode === k ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] font-semibold" : "text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
                <Ic size={15} /> {t(`mode.${k}`)}
              </button>
            ))}
          </div>
          <span className="text-[12px] text-[var(--color-muted)] truncate hidden 2xl:inline">{t(`mode.${mode}_desc`)}</span>
          <div className="flex items-center gap-1.5 shrink-0" title={t("comp.hint")}>
            <select value={p.subs.mode} onChange={(e) => branch("subs_content", { value: e.target.value })}
              className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-md px-2 py-1 text-[12px] text-[var(--color-text)] focus:border-[var(--color-accent)] focus:outline-none">
              <option value="none">{t("comp.subsNone")}</option>
              <option value="transcribe">{t("comp.subsOriginal")}</option>
              <option value="translate">{t("comp.subsTranslate")}</option>
            </select>
            <button onClick={() => branch("subs_burn", { on: p.subs.burn === false })} title={t("comp.burnHint")}
              className={`px-2.5 py-1 rounded-md text-[12px] border transition-colors ${p.subs.burn !== false ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] border-transparent font-medium" : "border-[var(--color-border)] text-[var(--color-muted)]"}`}>
              {t("comp.burn")}
            </button>
          </div>
          <div className="flex-1" />
          <button onClick={doUndo} disabled={!canUndo} title="Ctrl+Z"
            className="p-1.5 rounded-md text-[var(--color-muted)] hover:text-[var(--color-text)] disabled:opacity-30 transition-colors"><Undo2 size={16} /></button>
          <button onClick={doRedo} disabled={!canRedo} title="Ctrl+Shift+Z"
            className="p-1.5 rounded-md text-[var(--color-muted)] hover:text-[var(--color-text)] disabled:opacity-30 transition-colors"><Redo2 size={16} /></button>
          <button onClick={doExport} disabled={rendering}
            className="inline-flex items-center gap-2 px-4 py-1.5 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-70 hover:brightness-105 transition shrink-0">
            {rendering ? <Loader2 size={16} className="animate-spin" /> : <Download size={16} />}{t("export.proceed")}
          </button>
        </div>
        <div className="flex-1 min-h-0 p-3 overflow-hidden">
          {compare ? (
            <div className="w-full h-full grid grid-cols-2 gap-2 min-h-0">
              <ComparePane label={t("compare.original")} src={api.originalUrl(pid, scrub)} />
              <ComparePane label={t("compare.result")} src={api.previewUrl(pid, scrub, rev)} />
            </div>
          ) : (
            <PreviewCanvas pid={pid} project={p} scrub={scrub} rendered={rendered} lane={lane}
              onChanged={(fresh) => setProject(fresh)} />
          )}
        </div>
        <div className="flex items-center gap-3 px-4 py-2.5 border-t border-[var(--color-border)] bg-[var(--color-surface)]">
          <button onClick={playFull} title={t("play.dub")}
            className={`shrink-0 p-1.5 rounded-md transition-colors ${play ? "bg-[var(--color-accent)] text-[var(--color-on-accent)]" : "bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
            {play ? <Pause size={15} /> : <Play size={15} />}
          </button>
          <audio ref={audioRef} src={api.dubUrl(pid, dubRev)} onEnded={() => setPlay(false)} preload="auto" className="hidden" />
          <div className="flex items-center gap-1.5 shrink-0" title={t("play.volume")}>
            <Music size={13} className="text-[var(--color-muted)]" />
            <input type="range" min={0} max={1} step={0.02} value={vol}
              onChange={(e) => { const v = parseFloat(e.target.value); setVol(v); if (audioRef.current) audioRef.current.volume = v; localStorage.setItem("dub-vol", String(v)); }}
              className="w-16 accent-[var(--color-accent)]" />
          </div>
          <button onClick={() => setCompare((c) => !c)} title={t("compare.toggle")}
            className={`shrink-0 p-1.5 rounded-md transition-colors ${compare ? "bg-[var(--color-accent)] text-[var(--color-on-accent)]" : "bg-[var(--color-surface-2)] text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
            <Columns2 size={15} />
          </button>
          <span className="mono text-[11px] tabnum w-24 shrink-0"><span className="text-[var(--color-accent)] font-semibold">{fmtT(scrub)}</span><span className="text-[var(--color-muted)]"> / {fmtT(p.meta.duration || 0)}</span></span>
          <div className="flex-1 min-w-0">
            <WaveformTimeline pid={pid} duration={p.meta.duration || 0} scrub={scrub} segments={p.segments}
              gainDb={gainDraft ?? p.audio.gain_db ?? 0}
              onSeek={(t) => { setRendered(false); setScrub(t); if (audioRef.current) audioRef.current.currentTime = t; }} />
          </div>
        </div>
      </main>

      <aside className="border-l border-[var(--color-border)] overflow-y-auto p-4 bg-[var(--color-surface)] text-sm">
        <SectionLabel>{t("preset.title")}</SectionLabel>
        <div className="grid grid-cols-2 gap-1.5 mb-5 max-h-52 overflow-y-auto pr-1">
          <button onClick={() => branch("preset", { name: "" })}
            className={`text-[11px] px-2 py-1.5 rounded-lg border transition-colors ${!p.captions.preset?.name ? "border-[var(--color-accent)] text-[var(--color-accent)] bg-[color-mix(in_oklab,var(--color-accent)_8%,transparent)]" : "border-[var(--color-border)] text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
            {t("preset.original")}
          </button>
          {Object.keys(presets).map((name) => (
            <button key={name} onClick={() => branch("preset", { name })}
              className={`text-[11px] px-2 py-1.5 rounded-lg border truncate transition-colors ${p.captions.preset?.name === name ? "border-[var(--color-accent)] text-[var(--color-accent)] bg-[color-mix(in_oklab,var(--color-accent)_8%,transparent)]" : "border-[var(--color-border)] text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>
              {name}
            </button>
          ))}
        </div>
        <SectionLabel>{t("editor.style")}</SectionLabel>
        {ss && (
          <div className="space-y-3">
            <Row label={t("style.font")}>
              <select value={ss.font || "Montserrat"} onChange={(e) => branch("caption", { font: e.target.value })}
                className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-md px-2 py-1 text-[12px] max-w-[160px] focus:border-[var(--color-accent)] focus:outline-none transition-colors"
                title={fonts[ss.font || "Montserrat"] || ""}>
                {Object.keys(fonts).length ? Object.keys(fonts).map((f) => <option key={f} value={f}>{f}</option>)
                                           : <option value={ss.font || "Montserrat"}>{ss.font || "Montserrat"}</option>}
              </select>
            </Row>
            <Toggle label={t("style.bold")} on={ss.bold} onClick={() => branch("caption", { bold: !ss.bold })} />
            <Toggle label={t("style.italic")} on={ss.italic} onClick={() => branch("caption", { italic: !ss.italic })} />
            <Toggle label={t("style.caps")} on={ss.uppercase} onClick={() => branch("caption", { uppercase: !ss.uppercase })} />
            <Row label={t("style.color")}>
              <input type="color" value={ss.color} onChange={(e) => branch("caption", { color: e.target.value })}
                className="bg-transparent w-8 h-6 rounded cursor-pointer" />
            </Row>
            <Row label={t("style.outline")}>
              <div className="flex items-center gap-1.5">
                <input type="color" value={ss.outline || "#000000"} onChange={(e) => branch("caption", { outline: e.target.value })}
                  title={t("style.outline")} className="bg-transparent w-8 h-6 rounded cursor-pointer" />
                <input key={`sow-${ss.outline_w ?? "a"}`} type="number" min={0} max={20} defaultValue={ss.outline_w ?? undefined}
                  placeholder={t("style.outlineW")} title={t("style.outlineWFull")}
                  onBlur={(e) => branch("caption", { outline_w: e.target.value === "" ? null : parseInt(e.target.value) })}
                  className="w-12 bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none" />
                <select value={ss.shadow_dir ?? ""} title={t("style.shadow")}
                  onChange={(e) => branch("caption", { shadow_dir: e.target.value === "" ? null : parseInt(e.target.value) })}
                  className="bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded px-1 py-0.5 text-[11px] focus:border-[var(--color-accent)] focus:outline-none">
                  <option value="">—</option><option value="270">↑</option><option value="315">↗</option><option value="0">→</option><option value="45">↘</option><option value="90">↓</option><option value="135">↙</option><option value="180">←</option><option value="225">↖</option>
                </select>
              </div>
            </Row>
            <div>
              <div className="flex items-center justify-between">
                <span className="text-[var(--color-muted)]">{t("style.size")}</span>
                <span className="mono text-[11px] text-[var(--color-text)]">{sizeDraft ?? ss.size_px ?? Math.round((p.meta.height || 1280) / 14)}px</span>
              </div>
              <input type="range" min={24} max={Math.round((p.meta.height || 1280) / 5)}
                value={sizeDraft ?? ss.size_px ?? Math.round((p.meta.height || 1280) / 14)}
                onChange={(e) => setSizeDraft(parseInt(e.target.value))}
                onPointerUp={async () => { if (sizeDraft != null) { await branch("caption", { size_px: sizeDraft }); setSizeDraft(null); } }}
                className="w-full mt-1.5 accent-[var(--color-accent)]" />
            </div>
          </div>
        )}
        {mode === "funny" && (
          <div className="mt-6">
            <SectionLabel>{t("remix.title")}</SectionLabel>
            <textarea value={remixText} onChange={(e) => setRemixText(e.target.value)} rows={2}
              placeholder={t("remix.placeholder")}
              className="w-full bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-lg px-2.5 py-2 text-[13px] resize-none focus:border-[var(--color-accent)] focus:outline-none transition-colors" />
            <button onClick={doRemix} disabled={remixing || !remixText.trim()}
              className="mt-2 w-full inline-flex items-center justify-center gap-2 px-3 py-2 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-50 hover:brightness-105 transition">
              {remixing ? <Loader2 size={15} className="animate-spin" /> : <Sparkles size={15} />}{t("remix.apply")}
            </button>
            <div className="mt-1.5 text-[11px] text-[var(--color-muted)] leading-snug">{t("remix.hint")}</div>
          </div>
        )}
        {mode !== "subtitles" && (
          <>
            <div className="mt-6"><SectionLabel>{t("editor.voice")}</SectionLabel></div>
            <select value={p.audio.voice.mode} onChange={(e) => branch("recast", { voice_mode: e.target.value, voice_name: p.audio.voice.name })}
              className="w-[200px] max-w-full bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-lg px-2.5 py-2 focus:border-[var(--color-accent)] focus:outline-none transition-colors">
              <option value="clone">{t("voice.clone")}</option>
              <option value="autocast">{t("voice.autocast")}</option>
              <option value="voice">{t("voice.pack")}</option>
            </select>
            <VoiceRecorder voiceList={voiceList} onVoices={setVoiceList}
              onDone={(nm) => branch("recast", { voice_mode: "voice", voice_name: nm })} />
            {(() => {
              const spks = [...new Set(p.segments.map((s) => s.speaker ?? "0"))].sort();
              if (spks.length === 0) return null;
              return (
                <div className="mt-2">
                  <div className="text-[10px] text-[var(--color-muted)] mb-1">{t("voice.fromSpeakers")}</div>
                  <div className="flex flex-wrap gap-1.5">
                    {spks.map((spk) => (
                      <button key={spk} disabled={spkVoiceBusy !== null}
                        onClick={async () => {
                          setSpkVoiceBusy(spk);
                          try { const r = await api.speakerVoice(pid!, spk, `Спикер ${spk}`); if (r.ok) { setVoiceList(r.voices); branch("recast", { voice_mode: "voice", voice_name: r.name }); } } catch { /* ignore */ } finally { setSpkVoiceBusy(null); }
                        }}
                        className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-lg border border-[var(--color-border)] text-[12px] hover:border-[var(--color-accent)] disabled:opacity-40">
                        {spkVoiceBusy === spk ? <Loader2 size={12} className="animate-spin" /> : <AudioLines size={12} className="text-[var(--color-accent)]" />}SPK {spk}
                      </button>
                    ))}
                  </div>
                </div>
              );
            })()}
            {p.audio.voice.mode !== "voice" && (
              <div className="mt-1 text-[10px] text-[var(--color-muted)] leading-snug">{t("voice.recordHint")}</div>
            )}
            {p.audio.voice.mode === "voice" && (() => {
              // per-speaker pack voices: engine maps a comma-list to sorted speakers (cycling), so each
              // diarized speaker can get a DISTINCT/funny voice. 1 speaker -> a single picker.
              const spks = [...new Set(p.segments.map((s) => s.speaker ?? "0"))].sort();   // lexical — matches engine sorted()
              const names = (p.audio.voice.name || "").split(",").map((s) => s.trim());
              const pick = (cur: string, on: (v: string) => void) => (
                <div className="flex items-center gap-1.5">
                  <select value={cur} onChange={(e) => on(e.target.value)}
                    className="w-[200px] max-w-full bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-lg px-2.5 py-1.5 text-[13px] focus:border-[var(--color-accent)] focus:outline-none transition-colors">
                    <option value="">{voiceList.length ? "—" : "(пак не найден)"}</option>
                    {voiceList.map((v) => <option key={v} value={v}>{v}</option>)}
                  </select>
                  <button type="button" disabled={!cur} onClick={() => toggleVoicePreview(cur)} title={t("voice.preview")}
                    className="shrink-0 w-8 h-8 inline-flex items-center justify-center rounded-lg border border-[var(--color-border)] hover:border-[var(--color-accent)] disabled:opacity-40 disabled:cursor-not-allowed transition-colors">
                    {voicePreview === cur && cur ? <Pause size={14} className="text-[var(--color-accent)]" /> : <Play size={14} />}
                  </button>
                </div>
              );
              if (spks.length <= 1)
                return <div className="mt-2 flex">{pick(names[0] || "", (v) => branch("recast", { voice_mode: "voice", voice_name: v }))}</div>;
              return (
                <div className="mt-2 space-y-1.5">
                  {spks.map((spk, i) => (
                    <div key={spk} className="flex items-center gap-2">
                      <span className="mono text-[10px] text-[var(--color-muted)] w-12 shrink-0">SPK {spk}</span>
                      {pick(names[i] || "", (v) => branch("recast", {
                        voice_mode: "voice",
                        voice_name: spks.map((_, j) => (j === i ? v : names[j] || "")).join(","),
                      }))}
                    </div>
                  ))}
                </div>
              );
            })()}
            <div className="mt-3">
              <div className="flex items-center justify-between text-[11px] mb-1">
                <span className="text-[var(--color-muted)]">{t("voice.gain")}</span>
                <span className="mono text-[11px] text-[var(--color-text)]">{(gainDraft ?? p.audio.gain_db ?? 0) > 0 ? "+" : ""}{(gainDraft ?? p.audio.gain_db ?? 0).toFixed(1)} dB</span>
              </div>
              <input type="range" min={-12} max={12} step={0.5} value={gainDraft ?? p.audio.gain_db ?? 0}
                onChange={(e) => setGainDraft(parseFloat(e.target.value))}
                onPointerUp={async () => { if (gainDraft != null) { await doGain(gainDraft); setGainDraft(null); } }}
                className="w-full accent-[var(--color-accent)]" />
              <div className="text-[10px] text-[var(--color-muted)] leading-snug mt-0.5">{t("voice.gainHint")}</div>
            </div>
            {p.mode === "voiceover" && (   // закадровый: громкость ОРИГИНАЛЬНОЙ дорожки под переводом (0 = в полную силу, ниже = тише)
              <div className="mt-3">
                <div className="flex items-center justify-between text-[11px] mb-1">
                  <span className="text-[var(--color-muted)]">{t("voice.origGain")}</span>
                  <span className="mono text-[11px] text-[var(--color-text)]">{(voGainDraft ?? p.audio.voiceover_gain_db ?? -6).toFixed(1)} dB</span>
                </div>
                <input type="range" min={-24} max={0} step={0.5} value={voGainDraft ?? p.audio.voiceover_gain_db ?? -6}
                  onChange={(e) => setVoGainDraft(parseFloat(e.target.value))}
                  onPointerUp={async () => { if (voGainDraft != null) { await doVoiceoverGain(voGainDraft); setVoGainDraft(null); } }}
                  className="w-full accent-[var(--color-accent)]" />
                <div className="text-[10px] text-[var(--color-muted)] leading-snug mt-0.5">{t("voice.origGainHint")}</div>
              </div>
            )}
            <button onClick={doRegenAll} disabled={regenId !== null} title={t("voice.regenAll")}
              className="mt-3 w-full inline-flex items-center justify-center gap-2 px-3 py-2 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-50 hover:brightness-105 transition">
              {regenId === "__all__" ? <Loader2 size={15} className="animate-spin" /> : <RotateCw size={15} />}{t("voice.regenAll")}
            </button>
          </>
        )}

      </aside>
      <CommandPalette commands={cmds} />
    </div>
  );
}

// Поп-ап доп-голосов (HF-датасет): поиск, фильтр по полу, прослушка (play mp3), скачка в каталог.
function VoicePackModal({ have, onVoices, onClose }: { have: string[]; onVoices: (v: string[]) => void; onClose: () => void }) {
  const { t } = useTranslation();
  const [all, setAll] = useState<{ name: string; gender: string; url: string }[] | null>(null);
  const [q, setQ] = useState("");
  const [gender, setGender] = useState<"all" | "female" | "male">("all");
  const [playing, setPlaying] = useState<string | null>(null);
  const [getting, setGetting] = useState<string | null>(null);
  const audio = useRef<HTMLAudioElement | null>(null);
  useEffect(() => { api.voicesCatalog().then((r) => setAll(r.voices)).catch(() => setAll([])); }, []);
  useEffect(() => () => { audio.current?.pause(); }, []);

  const play = (url: string, name: string) => {
    audio.current?.pause();
    if (playing === name) { setPlaying(null); return; }
    const a = new Audio(url); audio.current = a; setPlaying(name);
    a.onended = () => setPlaying(null); a.play().catch(() => setPlaying(null));
  };
  const get = async (name: string) => {
    setGetting(name);
    try { const r = await api.voicesGet(name); if (r.ok && r.voices) onVoices(r.voices); } catch { /* ignore */ } finally { setGetting(null); }
  };
  const pretty = (n: string) => n.replace(/^RU_(Female|Male)_/, "").replace(/_/g, " ");
  const filtered = (all || []).filter((v) =>
    (gender === "all" || v.gender === gender) &&
    (!q.trim() || pretty(v.name).toLowerCase().includes(q.trim().toLowerCase()))
  );

  return (
    <div className="fixed inset-0 z-[60] grid place-items-center glass-scrim anim-fade" onClick={onClose}>
      <div className="w-[min(94vw,560px)] h-[min(82vh,640px)] flex flex-col rounded-xl glass-panel anim-pop p-5" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-3">
          <span className="font-semibold">{t("voice.packTitle")} {all && <span className="mono text-[11px] text-[var(--color-muted)]">{filtered.length}/{all.length}</span>}</span>
          <button onClick={onClose} className="text-[var(--color-muted)] hover:text-[var(--color-text)]"><X size={16} /></button>
        </div>
        <div className="flex items-center gap-2 mb-3">
          <input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("voice.search")} autoFocus
            className="flex-1 bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-lg px-2.5 py-1.5 text-[13px] focus:border-[var(--color-accent)] focus:outline-none" />
          <div className="flex rounded-lg border border-[var(--color-border)] overflow-hidden text-[12px]">
            {(["all", "female", "male"] as const).map((g) => (
              <button key={g} onClick={() => setGender(g)}
                className={`px-2.5 py-1.5 ${gender === g ? "bg-[var(--color-accent)] text-[var(--color-on-accent)]" : "text-[var(--color-muted)] hover:text-[var(--color-text)]"}`}>{t(`voice.g_${g}`)}</button>
            ))}
          </div>
        </div>
        <div className="flex-1 overflow-y-auto -mr-2 pr-2 space-y-1">
          {all === null ? <div className="mono text-[11px] text-[var(--color-muted)]">…</div> :
           filtered.slice(0, 300).map((v) => {
            const owned = have.includes(v.name) || have.includes(v.name.split("/").pop() || v.name);
            return (
              <div key={v.name} className="flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-[var(--color-surface-2)] border border-[var(--color-border)]">
                <span className={`w-1.5 h-1.5 rounded-full shrink-0 ${v.gender === "female" ? "bg-pink-400" : "bg-sky-400"}`} />
                <span className="text-[12px] truncate flex-1">{pretty(v.name)}</span>
                <button onClick={() => play(v.url, v.name)} title={t("voice.preview")}
                  className="shrink-0 p-1 rounded-md text-[var(--color-muted)] hover:text-[var(--color-accent)]">{playing === v.name ? <Pause size={14} /> : <Play size={14} />}</button>
                {owned ? <Check size={14} className="text-[var(--color-accent)] shrink-0 w-7 text-center" /> :
                  <button onClick={() => get(v.name)} disabled={!!getting} title={t("setup.downloadOne")}
                    className="shrink-0 p-1 rounded-md text-[var(--color-accent-2)] hover:text-[var(--color-accent)] disabled:opacity-40">
                    {getting === v.name ? <Loader2 size={14} className="animate-spin" /> : <Download size={14} />}
                  </button>}
              </div>
            );
          })}
          {all !== null && filtered.length > 300 && <div className="mono text-[10px] text-[var(--color-muted)] text-center py-2">{t("voice.narrowSearch")}</div>}
        </div>
      </div>
    </div>
  );
}

// Запись своего голоса с микрофона (авто-нейминг) + доп-голоса. onDone(name) — новый голос готов.
function VoiceRecorder({ voiceList, onVoices, onDone }: { voiceList: string[]; onVoices: (v: string[]) => void; onDone: (name: string) => void }) {
  const { t } = useTranslation();
  const [rec, setRec] = useState(false);
  const [level, setLevel] = useState(0);
  const [pack, setPack] = useState(false);
  const [rename, setRename] = useState<{ from: string; to: string } | null>(null);
  const lvlTimer = useRef<number | null>(null);

  const start = async () => {
    const auto = `Голос ${voiceList.length + 1}`; // авто-имя, переименовать можно после
    const r = await api.recordStart(auto).catch(() => null);
    if (!r || !r.ok) return;
    setRec(true);
    lvlTimer.current = window.setInterval(async () => {
      try { const l = await api.recordLevel(); setLevel(l.level); } catch { /* ignore */ }
    }, 100);
  };
  const stop = async () => {
    if (lvlTimer.current) window.clearInterval(lvlTimer.current);
    setRec(false); setLevel(0);
    const r = await api.recordStop().catch(() => null);
    if (r) { onVoices(r.voices); if (r.name) { onDone(r.name); setRename({ from: r.name, to: r.name }); } }
  };
  const applyRename = async () => {
    if (!rename || !rename.to.trim() || rename.to === rename.from) { setRename(null); return; }
    const r = await api.voicesRename(rename.from, rename.to).catch(() => null);
    if (r) { onVoices(r.voices); onDone(rename.to.trim()); }
    setRename(null);
  };

  return (
    <div className="mt-2 space-y-2">
      <div className="flex items-center gap-2">
        {rec ? (
          <button onClick={stop} className="flex-1 inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg bg-[var(--color-danger,#ef4444)] text-white text-[12px] font-semibold">
            <Square size={13} />{t("voice.recordStop")}
          </button>
        ) : (
          <button onClick={start} title={t("voice.record")} className="flex-1 inline-flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg border border-[var(--color-border)] text-[12px] font-medium hover:border-[var(--color-accent)]">
            <span className="w-2 h-2 rounded-full bg-[var(--color-danger,#ef4444)]" />{t("voice.record")}
          </button>
        )}
        <button onClick={() => setPack(true)} title={t("voice.packTitle")}
          className="shrink-0 inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-[var(--color-border)] text-[12px] text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)]">
          <Download size={13} />{t("voice.more")}
        </button>
      </div>
      {rec && (
        <div className="h-1.5 rounded-full bg-white/10 overflow-hidden">
          <div className="h-full bg-[var(--color-accent)] transition-[width] duration-100" style={{ width: `${Math.min(100, level * 140)}%` }} />
        </div>
      )}
      {rename && !rec && (
        <div className="flex items-center gap-2">
          <input value={rename.to} onChange={(e) => setRename({ ...rename, to: e.target.value })} autoFocus
            onKeyDown={(e) => { if (e.key === "Enter") applyRename(); }}
            placeholder={t("voice.recordName")}
            className="flex-1 min-w-0 bg-[var(--color-surface-2)] border border-[var(--color-border)] rounded-lg px-2.5 py-1.5 text-[12px] focus:border-[var(--color-accent)] focus:outline-none" />
          <button onClick={applyRename} className="shrink-0 px-2.5 py-1.5 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-[12px] font-semibold"><Check size={13} /></button>
        </div>
      )}
      {pack && <VoicePackModal have={voiceList} onVoices={onVoices} onClose={() => setPack(false)} />}
    </div>
  );
}

// Non-blocking export queue: a floating Files panel (bottom-right). You keep editing while renders run; each
// finished file gets Download + Open. Subscribes only to `exports`, so it repaints independently of the editor.
function FilesPanel() {
  const { t } = useTranslation();
  const exports = useStore((s) => s.exports);
  const [, force] = useState(0);
  const [dled, setDled] = useState<string | null>(null);       // id только что скачанного -> показать «Сохранено» (обратная связь)
  const [opening, setOpening] = useState<string | null>(null); // id открываемого файла
  const fl = useFloatable("files", { x: window.innerWidth - 360, y: 64 });
  const prevLen = useRef(0);
  useEffect(() => { force((n) => n + 1); }, []); // дождаться #dock-slot
  useEffect(() => { // первый экспорт -> раскрыть панель во float
    if (prevLen.current === 0 && exports.length > 0 && !fl.floating) fl.pop();
    prevLen.current = exports.length;
  }, [exports.length]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!exports.length) return null;
  const active = exports.filter((e) => e.status === "rendering").length;

  // докнутый чип в шапке
  if (!fl.floating) {
    const slot = dockSlot();
    if (!slot) return null;
    return createPortal(
      <button onClick={fl.pop} title={t("files.title")}
        className="inline-flex items-center gap-2 px-2.5 h-8 rounded-md bg-[var(--color-surface-2)] border border-[var(--color-border)] hover:border-[var(--color-accent)] transition-colors">
        {active ? <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" /> : <FolderDown size={14} className="text-[var(--color-accent)]" />}
        <span className="text-[12px] font-medium hidden sm:inline">{t("files.title")}</span>
        <span className="mono text-[11px] text-[var(--color-muted)]">{exports.length}</span>
      </button>,
      slot,
    );
  }

  // оторванная драг-панель
  return (
    <div className="fixed z-50 w-[min(90vw,340px)]" style={{ left: fl.pos.x, top: fl.pos.y }}>
      {(
        <div className="w-full rounded-xl glass-panel anim-pop overflow-hidden">
          <div onPointerDown={fl.onDragStart}
            className={`flex items-center justify-between px-3 py-2 border-b border-[var(--color-border)] ${fl.dragging ? "cursor-grabbing" : "cursor-grab"}`}>
            <span className="flex items-center gap-1.5 mono text-[11px] uppercase tracking-[0.14em] text-[var(--color-muted)]"><Move size={12} />{t("files.title")}</span>
            <button onClick={fl.dock} title="Вернуть в шапку" className="text-[var(--color-muted)] hover:text-[var(--color-text)]"><Minimize2 size={14} /></button>
          </div>
          <div className="max-h-[52vh] overflow-y-auto p-2 space-y-1.5">
            {exports.map((e) => (
              <div key={e.id} className="rounded-lg bg-[var(--color-surface-2)]/60 p-2.5">
                <div className="flex items-center gap-2">
                  {e.status === "rendering" ? <Loader2 size={14} className="animate-spin text-[var(--color-accent)] shrink-0" />
                    : e.status === "error" ? <span className="text-[var(--color-warn)] shrink-0 font-bold">!</span>
                    : <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-accent)] shrink-0" />}
                  <span className="text-[13px] truncate flex-1">{e.name}</span>
                </div>
                {e.status === "rendering" && (
                  <>
                    <div className="mt-1.5 h-1 w-full rounded-full bg-[var(--color-surface-2)] overflow-hidden"><div className="h-full w-1/3 rounded-full bg-[var(--color-accent)] anim-indeterminate" /></div>
                    <div className="mt-1 mono text-[10px] text-[var(--color-muted)] truncate">{e.msg}</div>
                  </>
                )}
                {e.status === "done" && e.url && (
                  <div className="mt-2 flex gap-1.5">
                    <a href={`${e.url}&dl=1`} download
                       onClick={() => { setDled(e.id); window.setTimeout(() => setDled((c) => (c === e.id ? null : c)), 2500); }}
                       className="flex-1 inline-flex items-center justify-center gap-1.5 text-[12px] py-1 rounded-md bg-[var(--color-accent)] text-[var(--color-on-accent)] font-semibold">
                      {dled === e.id ? <><Check size={13} /> {t("files.downloaded")}</> : <><Download size={13} /> {t("files.download")}</>}
                    </a>
                    <button onClick={async () => { if (!e.pid) return; setOpening(e.id); try { await api.openOutput(e.pid); } catch { /* ignore */ } finally { window.setTimeout(() => setOpening((c) => (c === e.id ? null : c)), 800); } }}
                       className="inline-flex items-center justify-center gap-1.5 text-[12px] px-2.5 py-1 rounded-md border border-[var(--color-border)] text-[var(--color-muted)] hover:text-[var(--color-text)]">
                      {opening === e.id ? <Loader2 size={13} className="animate-spin" /> : <ExternalLink size={13} />} {t("files.open")}
                    </button>
                  </div>
                )}
                {e.status === "error" && <div className="mt-1 mono text-[10px] text-[var(--color-warn)] truncate">{e.msg}</div>}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// human byte size (GB/MB) for the setup component list
function fmtBytes(n: number) {
  if (n >= 1e9) return `${(n / 1e9).toFixed(1)} GB`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(0)} MB`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(0)} KB`;
  return `${n} B`;
}

// Семейства квантов: разные варианты ОДНОЙ модели (выбор одного, «ИЛИ»). Явная карта по id — надёжнее
// префикса (higgs — квант TTS, higgs-engine — движок, это РАЗНЫЕ вещи). Остальные компоненты — одиночные.
const QUANT_GROUP: Record<string, string> = {
  higgs: "higgs", "higgs-q6_k": "higgs", "higgs-q4_k_m": "higgs",
  gemma: "gemma", "gemma-q5_0": "gemma", "gemma-q6_k": "gemma", "gemma-q8_0": "gemma",
  parakeet: "parakeet", "parakeet-fp32": "parakeet",
  roformer: "roformer", "roformer-q5": "roformer", "roformer-q4": "roformer",
};

// ── «Первый запуск»: панель автозакачки компонентов (модели/движки/системные библиотеки) ──
function FirstRun() {
  const { t } = useTranslation();
  const setStage = useStore((s) => s.setStage);
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [prog, setProg] = useState<{ pct: Record<string, number>; overall: number; msg: string } | null>(null); // pct[componentId] -> % (параллельные бары)
  const [err, setErr] = useState<string | null>(null);

  const refresh = async () => {
    const s = await api.setupStatus();
    setStatus(s);
    // preselect every missing downloadable component (кроме опциональных квантов — их качают вручную)
    setSel(new Set(s.components.filter((c) => c.delivery === "download" && !c.installed && c.requirement !== "optional").map((c) => c.id)));
    return s;
  };
  useEffect(() => { refresh().catch((e) => setErr(String(e))); }, []);

  async function download(ids: string[]) {
    if (ids.length === 0 || busy) return;
    setBusy(true); setErr(null); setProg({ pct: {}, overall: 0, msg: "" });
    try {
      const { job_id } = await api.setupDownload(ids);
      await api.watchJob(job_id, (e) => {
        if (e.type === "progress") {
          const m: Record<string, number> = {};
          (e.parts || []).forEach((p) => { m[p.component] = p.pct; });   // бар на каждый компонент
          setProg({ pct: m, overall: e.pct ?? 0, msg: e.msg || "" });
          useStore.getState().pushActivity(e.msg || "", "work");         // в общий журнал шапки
        }
      });
      const s = await refresh();
      if (s.ready) { setStage("empty"); playSfx("success"); }   // всё скачано -> сразу на стартовый экран, без ручного «Продолжить»
    } catch (e) {
      setErr(String(e)); useStore.getState().pushActivity(String(e), "error");
    } finally {
      setBusy(false); setProg(null);
    }
  }

  const toggle = (id: string) => setSel((prev) => { const n = new Set(prev); n.has(id) ? n.delete(id) : n.add(id); return n; });
  const selectedBytes = status ? status.components.filter((c) => sel.has(c.id)).reduce((a, c) => a + Math.max(0, c.size - c.bytesOnDisk), 0) : 0;

  const reqLabel = (r: string) => (r === "required" ? t("setup.required") : t("setup.recommended"));
  const deliveryNote = (c: SetupComponent) =>
    c.delivery === "bundled" ? t("setup.reinstallHint") : c.delivery === "external" ? t("setup.external") : "";
  const GROUP_LABEL: Record<string, string> = { higgs: t("setup.grpHiggs"), gemma: t("setup.grpGemma"), parakeet: t("setup.grpParakeet"), roformer: t("setup.grpRoformer") };
  const pickOne = (id: string, group: string) => setSel((prev) => {   // radio внутри семейства: выбрать этот квант, снять остальные того же семейства
    const n = new Set(prev);
    Object.entries(QUANT_GROUP).forEach(([cid, g]) => { if (g === group) n.delete(cid); });
    n.add(id);
    return n;
  });
  const comps = status?.components ?? [];
  const gseen = new Set<string>();
  type SetupRow = { kind: "one"; c: SetupComponent } | { kind: "group"; group: string; members: SetupComponent[] };
  const rows: SetupRow[] = comps.flatMap((c): SetupRow[] => {   // одиночные компоненты + по одной группе квантов (на первом её члене)
    const g = QUANT_GROUP[c.id];
    if (!g) return [{ kind: "one", c }];
    if (gseen.has(g)) return [];
    gseen.add(g);
    return [{ kind: "group", group: g, members: comps.filter((x) => QUANT_GROUP[x.id] === g) }];
  });

  return (
    <div className="flex-1 min-h-0 overflow-y-auto grid place-items-center px-6 py-10">
      <motion.div initial={{ opacity: 0, y: 14 }} animate={{ opacity: 1, y: 0 }} transition={{ duration: 0.5, ease: EASE }}
        className="w-full max-w-2xl">
        <div className="mono text-[11px] tracking-[0.2em] text-[var(--color-muted)] flex items-center gap-2">
          <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-accent)] shadow-[0_0_8px_var(--color-accent)]" />{t("setup.title")}
        </div>
        <h1 className="mt-4 text-2xl lg:text-[28px] leading-tight font-extrabold tracking-tight">{t("setup.title")}</h1>
        <p className="mt-3 text-[14px] leading-relaxed text-[var(--color-muted)]">{t("setup.subtitle")}</p>

        {err && (
          <div className="mt-4 rounded-lg border border-[var(--color-warn)]/40 bg-[color-mix(in_oklab,var(--color-warn)_10%,transparent)] px-3 py-2 text-[13px] text-[var(--color-warn)]">
            <span className="font-semibold">{t("setup.error")}</span> · <span className="mono text-[11px] break-words">{err}</span>
          </div>
        )}

        <div className="mt-4 rounded-lg border border-[var(--color-accent)]/30 bg-[color-mix(in_oklab,var(--color-accent)_8%,transparent)] px-3.5 py-2.5 text-[12.5px] leading-snug text-[var(--color-text)]">
          <span className="font-semibold text-[var(--color-accent)]">{t("setup.pickNoteTitle")}</span> {t("setup.pickNote")}
        </div>

        <div className="mt-4 space-y-2">
          {rows.map((row) => {
            if (row.kind === "group") {
              const { group, members } = row;
              return (
                <div key={group} className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] px-3.5 py-3">
                  <div className="flex items-center justify-between mb-2">
                    <span className="text-[13px] font-semibold">{GROUP_LABEL[group]}</span>
                    <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-[color-mix(in_oklab,var(--color-accent)_16%,transparent)] text-[var(--color-accent)]">{t("setup.pickOneQuant")}</span>
                  </div>
                  <div className="space-y-0.5">
                    {members.map((c) => {
                      const isDefault = c.requirement !== "optional";
                      const checked = sel.has(c.id);
                      const active = !!prog && prog.pct[c.id] != null;
                      return (
                        <label key={c.id} className={`flex items-center gap-2.5 rounded-lg px-2 py-1.5 ${c.installed ? "" : "cursor-pointer hover:bg-[var(--color-surface-2)]"} ${checked && !c.installed ? "bg-[color-mix(in_oklab,var(--color-accent)_8%,transparent)]" : ""}`}>
                          {c.installed
                            ? <span className="w-4 h-4 shrink-0 grid place-items-center text-[var(--color-accent)]"><Check size={15} strokeWidth={3} /></span>
                            : <input type="radio" name={`grp-${group}`} checked={checked} onChange={() => pickOne(c.id, group)} disabled={busy} className="w-4 h-4 accent-[var(--color-accent)] shrink-0" />}
                          <div className="min-w-0 flex-1">
                            <div className="flex items-center gap-2">
                              <span className="text-[13px] truncate">{c.name}</span>
                              {isDefault
                                ? <span className="text-[9px] px-1.5 py-0.5 rounded bg-[color-mix(in_oklab,var(--color-accent)_16%,transparent)] text-[var(--color-accent)] uppercase tracking-wide shrink-0">{t("setup.default")}</span>
                                : <span className="text-[9px] px-1.5 py-0.5 rounded bg-[var(--color-surface-2)] text-[var(--color-muted)] uppercase tracking-wide shrink-0">{t("setup.alt")}</span>}
                              {c.installed && <span className="text-[10px] text-[var(--color-accent)] shrink-0">{t("setup.installed")}</span>}
                            </div>
                            {active && <div className="mt-1 h-1 rounded-full bg-[var(--color-surface-2)] overflow-hidden"><div className="h-full bg-[var(--color-accent)]" style={{ width: `${prog!.pct[c.id] ?? 0}%` }} /></div>}
                          </div>
                          <span className="mono text-[11px] text-[var(--color-muted)] shrink-0">{fmtBytes(c.size)}{c.vram ? ` · ${fmtBytes(c.vram)} VRAM` : ""}</span>
                        </label>
                      );
                    })}
                  </div>
                  <div className="mt-1.5 text-[10px] text-[var(--color-muted)]">{t("setup.quantHint")}</div>
                </div>
              );
            }
            const c = row.c;
            const active = !!prog && prog.pct[c.id] != null;
            const canPick = c.delivery === "download" && !c.installed;
            return (
              <div key={c.id} className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] px-3.5 py-3">
                <div className="flex items-center gap-3">
                  {canPick ? (
                    <input type="checkbox" checked={sel.has(c.id)} onChange={() => toggle(c.id)} disabled={busy}
                      className="w-4 h-4 accent-[var(--color-accent)] shrink-0" />
                  ) : (
                    <span className={`w-4 h-4 shrink-0 grid place-items-center rounded ${c.installed ? "text-[var(--color-accent)]" : "text-[var(--color-muted)]"}`}>
                      {c.installed ? <Check size={16} strokeWidth={3} /> : <X size={14} />}
                    </span>
                  )}
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="font-semibold text-[14px] truncate">{c.name}</span>
                      <span className={`text-[10px] px-1.5 py-0.5 rounded uppercase tracking-wide ${c.requirement === "required" ? "bg-[color-mix(in_oklab,var(--color-accent)_16%,transparent)] text-[var(--color-accent)]" : "bg-[var(--color-surface-2)] text-[var(--color-muted)]"}`}>{reqLabel(c.requirement)}</span>
                    </div>
                    <div className="text-[12px] text-[var(--color-muted)] truncate">{c.purpose}</div>
                    {active && (
                      <div className="mt-1.5 h-1.5 rounded-full bg-[var(--color-surface-2)] overflow-hidden">
                        <div className="h-full bg-[var(--color-accent)] transition-[width] duration-200" style={{ width: `${prog!.pct[c.id] ?? 0}%` }} />
                      </div>
                    )}
                  </div>
                  <div className="text-right shrink-0">
                    {c.delivery === "external" ? (
                      c.installed ? <span className="text-[12px] text-[var(--color-accent)]">{t("setup.installed")}</span>
                        : <button onClick={() => c.externalUrl && window.open(c.externalUrl, "_blank")}
                            className="inline-flex items-center gap-1 text-[12px] text-[var(--color-accent-2)] hover:underline"><ExternalLink size={13} />{t("setup.openDriver")}</button>
                    ) : c.installed ? (
                      <span className="text-[12px] text-[var(--color-accent)]">{t("setup.installed")}</span>
                    ) : c.delivery === "bundled" ? (
                      <span className="text-[11px] text-[var(--color-muted)] max-w-[120px] inline-block leading-tight">{deliveryNote(c)}</span>
                    ) : (
                      <div className="flex flex-col items-end gap-1">
                        <span className="mono text-[11px] text-[var(--color-muted)]">{fmtBytes(c.size)}</span>
                        <button onClick={() => download([c.id])} disabled={busy}
                          className="inline-flex items-center gap-1 px-2 py-1 rounded-md bg-[var(--color-surface-2)] border border-[var(--color-border)] text-[11px] hover:border-[var(--color-accent)] disabled:opacity-40">
                          <Download size={12} />{t("setup.downloadOne")}</button>
                      </div>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>

        <div className="mt-6 flex items-center gap-3">
          {status && !status.ready && (
            <button onClick={() => download([...sel])} disabled={busy || sel.size === 0}
              className="inline-flex items-center gap-2 px-4 py-2.5 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-40 hover:brightness-105">
              {busy ? <Loader2 size={16} className="animate-spin" /> : <Download size={16} />}
              {t("setup.download")} {selectedBytes > 0 && <span className="opacity-80">· {fmtBytes(selectedBytes)}</span>}
            </button>
          )}
          {busy && (
            <button onClick={() => api.setupCancel().catch(() => {})}
              className="inline-flex items-center gap-1.5 px-3 py-2.5 rounded-lg border border-[var(--color-border)] text-sm text-[var(--color-muted)] hover:text-[var(--color-text)]">
              <Square size={14} />{t("setup.cancel")}</button>
          )}
          {!busy && (
            <button onClick={async () => { try { const r = await api.setupBrowse(); if (r.picked) { setStatus(r.status); setSel(new Set(r.status.components.filter((c) => c.delivery === "download" && !c.installed && c.requirement !== "optional").map((c) => c.id))); } } catch { /* ignore */ } }}
              className="inline-flex items-center gap-1.5 px-3 py-2.5 rounded-lg border border-dashed border-[var(--color-border)] text-sm text-[var(--color-muted)] hover:border-[var(--color-accent)] hover:text-[var(--color-text)] transition-colors">
              <FolderDown size={14} />{t("settings.browseFolder")}</button>
          )}
          {status?.ready && !busy && (
            <button onClick={() => setStage("empty")}
              className="inline-flex items-center gap-2 px-4 py-2.5 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold hover:brightness-105">
              <Check size={16} />{t("setup.continue")}</button>
          )}
          {busy && prog && <span className="mono text-[11px] text-[var(--color-muted)] truncate">{prog.msg} · {prog.overall.toFixed(0)}%</span>}
        </div>
      </motion.div>
    </div>
  );
}

// Пакетная обработка: DropZone кладёт выбранные файлы + настройки сюда, BatchView читает (без раздувания стора).
const batchState: { files: File[]; tgt: string; src: string; audio: string; subs: string; burn: boolean; funnyOn: boolean; funny: string } =
  { files: [], tgt: "ru", src: "auto", audio: "dub", subs: "translate", burn: true, funnyOn: false, funny: "" };

type BatchItem = { name: string; status: "queued" | "analyzing" | "rendering" | "done" | "error"; pid: string | null; pct: number; msg?: string };

// Пакетный режим: пачка файлов -> для каждого createProject -> analyze -> (dub/funny) render, последовательно
// (движок одно-воркерный, GPU сериализует). Переиспользует существующие эндпоинты, отдельного backend не нужно.
function BatchView() {
  const { t } = useTranslation();
  const setStage = useStore((s) => s.setStage);
  const filesRef = useRef<File[]>(batchState.files);
  const { tgt, src, audio, subs, burn, funnyOn, funny } = batchState;
  const [items, setItems] = useState<BatchItem[]>(() => filesRef.current.map((f) => ({ name: f.name, status: "queued", pid: null, pct: 0 })));
  const [running, setRunning] = useState(false);
  const [doneN, setDoneN] = useState(0);

  const eMode = audio;
  const eSubs = audio === "transcribe" ? "transcribe" : subs;
  const eRewrite = funnyOn && (audio === "dub" || audio === "voiceover") ? funny.trim() : "";
  const doRender = audio === "dub" || audio === "voiceover";

  async function start() {
    setRunning(true);
    for (let i = 0; i < filesRef.current.length; i++) {
      const upd = (patch: Partial<BatchItem>) => setItems((xs) => xs.map((x, j) => (j === i ? { ...x, ...patch } : x)));
      try {
        upd({ status: "analyzing", pct: 0 });
        const { project_id } = await api.createProject(filesRef.current[i]);
        upd({ pid: project_id });
        const { job_id } = await api.analyze(project_id, tgt, eMode, src, eSubs, eRewrite, burn);
        await api.watchJob(job_id, (e) => { if (e.type === "progress") upd({ pct: e.pct ?? 0 }); });
        if (doRender) {
          upd({ status: "rendering", pct: 0 });
          const r = await api.render(project_id);
          await api.watchJob(r.job_id, (e) => { if (e.type === "progress") upd({ pct: e.pct ?? 0 }); });
        }
        upd({ status: "done", pct: 100 });
      } catch (err) {
        upd({ status: "error", msg: String(err) });
      }
      setDoneN((n) => n + 1);
    }
    setRunning(false);
  }

  const icon = (s: BatchItem["status"]) =>
    s === "done" ? <Check size={15} className="text-[var(--color-accent)]" /> :
    s === "error" ? <X size={15} className="text-[var(--color-warn)]" /> :
    s === "queued" ? <Square size={13} className="text-[var(--color-muted)]" /> :
    <Loader2 size={15} className="animate-spin text-[var(--color-accent)]" />;

  return (
    <main className="flex-1 min-h-0 overflow-hidden flex flex-col p-4">
      <div className="max-w-3xl w-full mx-auto flex flex-col min-h-0">
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <button onClick={() => setStage("empty")} className="text-[13px] text-[var(--color-muted)] hover:text-[var(--color-text)] inline-flex items-center gap-1"><ArrowRight size={14} className="rotate-180" />{t("batch.back")}</button>
            <span className="text-[15px] font-semibold flex items-center gap-2"><FolderDown size={16} className="text-[var(--color-accent)]" />{t("batch.title")}</span>
          </div>
          <span className="text-[12px] text-[var(--color-muted)]">{doneN}/{items.length} · {items.length} {t("batch.files")}</span>
        </div>

        <div className="flex-1 min-h-0 overflow-y-auto rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] divide-y divide-[var(--color-border)]">
          {items.map((it, i) => (
            <div key={i} className="flex items-center gap-3 px-3 py-2.5">
              <span className="shrink-0">{icon(it.status)}</span>
              <div className="min-w-0 flex-1">
                <div className="text-[13px] truncate">{it.name}</div>
                {(it.status === "analyzing" || it.status === "rendering") && (
                  <div className="mt-1 h-1 rounded bg-[var(--color-surface-2)] overflow-hidden">
                    <div className="h-full bg-[var(--color-accent)] transition-all" style={{ width: `${Math.round((it.pct || 0) * 100)}%` }} />
                  </div>
                )}
                {it.status === "error" && <div className="text-[10px] text-[var(--color-warn)] truncate mono">{it.msg}</div>}
              </div>
              <span className="text-[11px] text-[var(--color-muted)] shrink-0">{t(`batch.${it.status}`)}</span>
              {it.status === "done" && it.pid && (
                <button onClick={() => it.pid && api.openOutput(it.pid).catch(() => {})} className="text-[11px] text-[var(--color-accent)] inline-flex items-center gap-1 shrink-0 hover:underline"><ExternalLink size={12} />{t("batch.open_out")}</button>
              )}
            </div>
          ))}
        </div>

        <button onClick={start} disabled={running || items.length === 0}
          className="mt-3 w-full inline-flex items-center justify-center gap-2 px-4 py-2.5 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-50 hover:brightness-105 transition">
          {running ? <Loader2 size={15} className="animate-spin" /> : <Play size={15} />}{t("batch.start")}
        </button>
      </div>
    </main>
  );
}

// Режим «Транскрипт»: диаризованный транскрипт (analyze mode=transcribe) + создание голосов из спикеров
// (speaker-voice, ref-текст авто-транскрибируется на рендере) + экспорт .srt/.txt. Отдельный экран, не Editor.
const SPK_PALETTE = ["#7fb3ff", "#f79bd3", "#c6f24e", "#ffb454"];   // до 4 спикеров

function TranscriptView() {
  const { t } = useTranslation();
  const p = useStore((s) => s.project) as Project;
  const pid = useStore((s) => s.pid) as string;
  const [busy, setBusy] = useState<string | null>(null);            // спикер в работе, либо "__all__"
  const [made, setMade] = useState<Record<string, string>>({});     // спикер -> имя созданного голоса
  const [scrub, setScrub] = useState(0);
  const [play, setPlay] = useState(false);
  const videoRef = useRef<HTMLVideoElement>(null);
  // Плей: <video> тянет /dub — для транскрипта (nodub) это ОРИГИНАЛ (видео+аудио). Скраб следует за
  // currentTime -> активная строка + караоке-подсветка слова. Один элемент = и картинка, и звук.
  useEffect(() => {
    const v = videoRef.current; if (!v) return;
    if (!play) { v.pause(); return; }
    v.play().catch(() => setPlay(false));
    const id = window.setInterval(() => setScrub(v.currentTime), 80);
    return () => window.clearInterval(id);
  }, [play]);
  const seek = (tt: number) => { setScrub(tt); if (videoRef.current) videoRef.current.currentTime = tt; };
  // пословные тайминги ASR (лежат в extra.words) — для караоке внутри активной фразы
  const wordsOf = (s: Project["segments"][number]) =>
    (((s as unknown as { extra?: { words?: Array<{ word: string; start: number; end: number }> } }).extra?.words) || []);

  const rows = p.segments.filter((s) => (s.src_text || "").trim());
  const speakers = [...new Set(p.segments.map((s) => s.speaker ?? "0"))].sort();
  const colorOf = (spk: string) => SPK_PALETTE[Math.max(0, speakers.indexOf(spk)) % SPK_PALETTE.length];
  // активная фраза = та, чей [start,end] накрывает скраб; при смене — подсветка + автоскролл к ней
  const activeId = rows.find((s) => scrub >= s.start && scrub < (s.end > s.start ? s.end : s.start + 3))?.id;
  const activeRef = useRef<HTMLDivElement>(null);
  useEffect(() => { activeRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" }); }, [activeId]);
  const durOf = (spk: string) => p.segments.filter((s) => (s.speaker ?? "0") === spk).reduce((a, s) => a + Math.max(0, s.end - s.start), 0);
  const fmt = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

  async function makeVoice(spk: string) {
    setBusy(spk);
    try { const r = await api.speakerVoice(pid, spk, `${t("transcribe.speaker")} ${spk}`); if (r.ok) setMade((m) => ({ ...m, [spk]: r.name })); }
    catch { /* ignore */ } finally { setBusy(null); }
  }
  async function makeAll() {
    setBusy("__all__");
    try { for (const spk of speakers) { const r = await api.speakerVoice(pid, spk, `${t("transcribe.speaker")} ${spk}`); if (r.ok) setMade((m) => ({ ...m, [spk]: r.name })); } }
    catch { /* ignore */ } finally { setBusy(null); }
  }

  function dl(name: string, text: string) {
    const blob = new Blob([text], { type: "text/plain;charset=utf-8" });
    const a = document.createElement("a"); a.href = URL.createObjectURL(blob); a.download = name; a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  }
  const srtTime = (s: number) => {
    const ms = Math.max(0, Math.round(s * 1000)), z = (n: number, w = 2) => String(n).padStart(w, "0");
    return `${z(Math.floor(ms / 3600000))}:${z(Math.floor((ms % 3600000) / 60000))}:${z(Math.floor((ms % 60000) / 1000))},${z(ms % 1000, 3)}`;
  };
  const exportSrt = () => dl("transcript.srt", rows.map((s, i) => `${i + 1}\n${srtTime(s.start)} --> ${srtTime(s.end)}\n${(s.src_text || "").trim()}\n`).join("\n"));
  const exportTxt = () => dl("transcript.txt", rows.map((s) => `[${t("transcribe.speaker")} ${s.speaker ?? "0"}] ${(s.src_text || "").trim()}`).join("\n"));

  return (
    <main className="flex-1 min-h-0 overflow-hidden grid grid-cols-[1.4fr_1fr] gap-3 p-3">
      <div className="min-h-0 flex flex-col rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)]">
        <div className="px-3 py-2 flex items-center justify-between border-b border-[var(--color-border)]">
          <span className="text-[13px] font-medium flex items-center gap-2 min-w-0"><FileText size={15} className="text-[var(--color-accent)] shrink-0" /><span className="truncate">{p.meta.video?.split(/[\\/]/).pop() || t("transcribe.title")}</span></span>
          <span className="text-[11px] text-[var(--color-muted)] shrink-0">{speakers.length} {t("transcribe.speakersN")}</span>
        </div>
        <div className="px-3 pt-2 space-y-2">
          <div className="relative rounded-lg overflow-hidden bg-black/50 border border-[var(--color-border)] grid place-items-center max-h-[34vh]">
            <video ref={videoRef} src={api.dubUrl(pid)} playsInline preload="auto"
              onEnded={() => setPlay(false)} onClick={() => setPlay((x) => !x)}
              className="max-h-[34vh] max-w-full cursor-pointer" />
            <button onClick={() => setPlay((x) => !x)} title={play ? t("common.pause") : t("common.play")}
              className="absolute bottom-2 left-2 grid place-items-center w-10 h-10 rounded-full bg-[var(--color-accent)] text-[var(--color-on-accent)] shadow-lg hover:brightness-110 transition">
              {play ? <Pause size={18} /> : <Play size={18} className="ml-0.5" />}
            </button>
          </div>
          <WaveformTimeline pid={pid} duration={p.meta.duration || 0} scrub={scrub} segments={p.segments} onSeek={seek} />
        </div>
        <div className="flex-1 min-h-0 overflow-y-auto px-3 py-2 space-y-1.5">
          {rows.map((s) => {
            const spk = s.speaker ?? "0";
            const active = s.id === activeId;
            const words = active ? wordsOf(s) : [];
            return (
              <div key={s.id} ref={active ? activeRef : undefined} onClick={() => seek(s.start)}
                className={`flex gap-2 items-start cursor-pointer rounded px-1.5 -mx-1.5 py-0.5 transition-colors ${active ? "bg-[color-mix(in_oklab,var(--color-accent)_16%,transparent)]" : "hover:bg-[var(--color-surface-2)]"}`}>
                <span className="mono text-[9px] px-1.5 py-px rounded shrink-0" style={{ background: colorOf(spk), color: "#0b0c0e" }}>SPK {spk}</span>
                <span className="mono text-[9px] text-[var(--color-muted)] pt-0.5 shrink-0 w-8">{fmt(s.start)}</span>
                <span className="text-[13px] leading-snug">
                  {active && words.length > 0
                    ? words.map((w, i) => (
                        <span key={i} className={`transition-colors ${scrub >= w.start && scrub < w.end ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] rounded px-0.5" : ""}`}>{w.word}{" "}</span>
                      ))
                    : (s.src_text || "").trim()}
                </span>
              </div>
            );
          })}
          {rows.length === 0 && <div className="text-[12px] text-[var(--color-muted)] py-6 text-center">{t("transcribe.empty")}</div>}
        </div>
      </div>

      <div className="min-h-0 flex flex-col gap-3">
        <div className="rounded-xl border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
          <div className="text-[12px] text-[var(--color-muted)] mb-2">{t("transcribe.speakers")}</div>
          <div>
            {speakers.map((spk) => (
              <div key={spk} className="flex items-center justify-between py-1.5 border-b border-[var(--color-border)] last:border-0">
                <div className="flex items-center gap-2">
                  <span className="w-2.5 h-2.5 rounded-full shrink-0" style={{ background: colorOf(spk) }} />
                  <div>
                    <div className="text-[13px]">{t("transcribe.speaker")} {spk}</div>
                    <div className="text-[10px] text-[var(--color-muted)]">{durOf(spk).toFixed(1)}{t("transcribe.sec")}{made[spk] ? ` · ${made[spk]}` : ""}</div>
                  </div>
                </div>
                <button onClick={() => makeVoice(spk)} disabled={busy !== null}
                  className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-lg border border-[#37414d] text-[11px] text-[var(--color-accent)] hover:border-[var(--color-accent)] disabled:opacity-40">
                  {busy === spk ? <Loader2 size={12} className="animate-spin" /> : made[spk] ? <Check size={12} /> : <Mic2 size={12} />}{t("transcribe.makeVoice")}
                </button>
              </div>
            ))}
          </div>
          <div className="text-[10px] text-[var(--color-muted)] mt-2 flex items-center gap-1.5"><Sparkles size={11} className="text-[var(--color-accent)] shrink-0" />{t("transcribe.refHint")}</div>
        </div>

        <button onClick={makeAll} disabled={busy !== null}
          className="w-full inline-flex items-center justify-center gap-2 px-3 py-2 rounded-lg bg-[var(--color-accent)] text-[var(--color-on-accent)] text-sm font-semibold disabled:opacity-50 hover:brightness-105 transition">
          {busy === "__all__" ? <Loader2 size={15} className="animate-spin" /> : <Users size={15} />}{t("transcribe.makeAll")}
        </button>
        <div className="flex gap-2">
          <button onClick={exportSrt} className="flex-1 inline-flex items-center justify-center gap-1.5 px-3 py-2 rounded-lg border border-[#37414d] text-[12px] hover:border-[var(--color-accent)]"><Download size={13} />{t("transcribe.exportSrt")}</button>
          <button onClick={exportTxt} className="flex-1 inline-flex items-center justify-center gap-1.5 px-3 py-2 rounded-lg border border-[#37414d] text-[12px] hover:border-[var(--color-accent)]"><FileText size={13} />{t("transcribe.exportTxt")}</button>
        </div>
      </div>
    </main>
  );
}

export default function App() {
  const stage = useStore((s) => s.stage);                 // only re-route on stage change (not on every store write)
  const projMode = useStore((s) => (s.project as Project | null)?.mode);   // transcribe -> отдельный экран
  const setPid = useStore((s) => s.setPid);
  const setProject = useStore((s) => s.setProject);
  const setStage = useStore((s) => s.setStage);
  const [cap, setCap] = useState("");
  useEffect(() => { api.capabilities().then((c) => setCap(`${c.device} · ${c.asr_model}`)).catch(() => setCap("backend offline")); }, []);
  // boot: resume ?pid=… project, else gate on /setup/status — missing required components -> «first run».
  useEffect(() => {
    const pid = new URLSearchParams(location.search).get("pid");
    if (pid) { api.getProject(pid).then((p) => { setPid(pid); setProject(p); setStage("editor"); }).catch(() => setStage("empty")); return; }
    api.setupStatus()
      .then((s) => setStage(s.ready ? "empty" : "setup"))
      .catch(() => setStage("empty"));   // backend offline / older server without /setup -> fall through to hero
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <div className="h-full flex flex-col">
      <TopBar />
      {stage === "boot" && <div className="flex-1 grid place-items-center"><Loader2 size={22} className="animate-spin text-[var(--color-muted)]" /></div>}
      {stage === "setup" && <FirstRun />}
      {stage === "empty" && <DropZone />}
      {stage === "analyzing" && <AnalyzeProgress />}
      {stage === "batch" && <BatchView />}
      {stage === "editor" && (projMode === "transcribe" ? <TranscriptView /> : <Editor />)}
      <footer className="mono h-6 px-4 flex items-center gap-2 text-[10px] text-[var(--color-muted)] border-t border-[var(--color-border)] bg-[var(--color-surface)]">
        <span className="w-1.5 h-1.5 rounded-full bg-[var(--color-accent)]" />{cap}
      </footer>
      <FilesPanel />
      {(stage === "editor" || stage === "analyzing") && <ResourceMonitor />}
    </div>
  );
}
