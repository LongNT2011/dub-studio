// Живое превью дубляжа БЕЗ пожадорного серверного рендера: нативное исходное <video muted> (плавно, 60fps,
// мгновенный скраб) + JASSUB (WASM-libass) рисует ТОТ ЖЕ ASS, что и финальный burn, поверх видео в Web
// Worker; блюр-регионы — CSS backdrop-filter (аппроксимация ffmpeg-гаусса). Dub-звук (audioRef) — мастер-часы:
// видео подтягивается к нему по дрейфу. «preview = аппроксимация, export = истина» (индустриальный паттерн).
import { useEffect, useRef, useState } from "react";
import JASSUB from "jassub";
import workerUrl from "jassub/dist/wasm/jassub-worker.js?url";
import wasmUrl from "jassub/dist/wasm/jassub-worker.wasm?url";
import modernWasmUrl from "jassub/dist/wasm/jassub-worker-modern.wasm?url";
import { api, type Project } from "../lib/api";

// Семейство ASS-шрифта -> bundled TTF (ключи в availableFonts — нижним регистром, libass матчит без регистра).
const FONT_FILES: Record<string, string> = {
  montserrat: "Montserrat.ttf",
  oswald: "Oswald.ttf",
  roboto: "Roboto.ttf",
  "russo one": "RussoOne-Regular.ttf",
  pacifico: "Pacifico-Regular.ttf",
  "playfair display": "PlayfairDisplay.ttf",
  caveat: "Caveat.ttf",
  anton: "Anton-Regular.ttf",
  "bebas neue": "BebasNeue-Regular.ttf",
  poppins: "Poppins-Bold.ttf",
  "league spartan": "LeagueSpartan.ttf",
};

export default function LivePreview({ pid, project, rev, audioRef, playing }: {
  pid: string;
  project: Project;
  rev: number;
  audioRef: React.RefObject<HTMLAudioElement | null>;
  playing: boolean;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const jassubRef = useRef<JASSUB | null>(null);
  const [t, setT] = useState(() => audioRef.current?.currentTime ?? 0); // время для блюр-дивов
  const vw = project.meta.width || 16;
  const vh = project.meta.height || 9;

  // JASSUB поверх видео с ЖИВЫМ ASS. Пересоздаём на смену rev (правка сабов/титров) — дёшево, т.к. живое
  // превью показывается только на плее (правки идут на паузе через konva-канвас).
  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    const availableFonts: Record<string, string> = {};
    for (const [fam, file] of Object.entries(FONT_FILES)) availableFonts[fam] = api.fontUrl(file);
    let inst: JASSUB | null = null;
    try {
      inst = new JASSUB({
        video,
        subUrl: api.subsAssUrl(pid, rev),
        workerUrl,
        wasmUrl,
        modernWasmUrl,
        availableFonts,
        defaultFont: "montserrat",
        queryFonts: false,          // оффлайн: не дёргать системные/удалённые шрифты
        maxRenderHeight: 1080,      // кап рендера сабов (на 4К дешевле, на превью незаметно)
      });
      jassubRef.current = inst;
    } catch (e) {
      console.error("JASSUB init failed", e);
    }
    return () => {
      inst?.destroy().catch(() => {});
      jassubRef.current = null;
    };
  }, [pid, rev]);

  // Синхрон: dub-аудио = мастер-часы. Видео играем и подтягиваем к аудио при дрейфе >150мс. t -> блюр-дивы.
  useEffect(() => {
    const video = videoRef.current;
    const a = audioRef.current;
    if (!video || !a) return;
    let raf = 0;
    const loop = () => {
      if (Math.abs(video.currentTime - a.currentTime) > 0.15) video.currentTime = a.currentTime;
      setT(a.currentTime);
      raf = requestAnimationFrame(loop);
    };
    if (playing) {
      try { video.currentTime = a.currentTime; } catch { /* not seekable yet */ }
      video.play().catch(() => {});
      raf = requestAnimationFrame(loop);
    } else {
      video.pause();
    }
    return () => { cancelAnimationFrame(raf); video.pause(); };
  }, [playing, audioRef]);

  // Блюр-боксы активные на текущем времени (окно как в burn). fill -> сплошная плашка, иначе CSS-блюр.
  const blurs = (project.captions.blur_boxes || []).filter(
    (b) => !b.hidden && t >= b.t0 - 0.05 && t <= b.t1 + 0.05
  );

  return (
    <div className="w-full h-full grid place-items-center bg-black/40 rounded-xl overflow-hidden">
      <div className="relative" style={{ aspectRatio: `${vw} / ${vh}`, height: "100%", width: "auto", maxWidth: "100%", maxHeight: "100%" }}>
        <video ref={videoRef} src={api.sourceVideoUrl(pid)} muted playsInline preload="auto"
               className="absolute inset-0 w-full h-full object-fill" />
        {blurs.map((b, i) => {
          const solid = !!b.fill;
          return (
            <div key={i} className="absolute pointer-events-none" style={{
              left: `${(b.x / vw) * 100}%`,
              top: `${(b.y / vh) * 100}%`,
              width: `${(b.w / vw) * 100}%`,
              height: `${(b.h / vh) * 100}%`,
              background: solid ? (b.fill as string) : "transparent",
              backdropFilter: solid ? undefined : "blur(10px)",
              WebkitBackdropFilter: solid ? undefined : "blur(10px)",
            }} />
          );
        })}
      </div>
    </div>
  );
}
