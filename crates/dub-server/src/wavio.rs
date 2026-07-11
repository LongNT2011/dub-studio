//! Мини WAV I/O для сборки таймлайна дубляжа (assemble.timeline): чтение WAV в mono f32 + запись
//! mono f32. Higgs отдаёт PCM f32 через audiocpp::encode_wav (PCM16 WAV) — hound читает оба формата,
//! стерео сводим в mono усреднением (как s.mean(axis=1) в питоне).

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::path::Path;

/// Прочитать WAV -> (mono f32 сэмплы, sample_rate).
pub fn read_mono_f32(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let mut r = WavReader::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        SampleFormat::Float => r
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("read f32: {e}"))?,
        SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("read int: {e}"))?
        }
    };
    let mono: Vec<f32> = if ch <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(ch)
            .map(|fr| fr.iter().sum::<f32>() / ch as f32)
            .collect()
    };
    Ok((mono, spec.sample_rate))
}

/// Записать mono f32 -> WAV IEEE-float32.
pub fn write_mono_f32(path: &Path, data: &[f32], sample_rate: u32) -> Result<(), String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut w = WavWriter::create(path, spec).map_err(|e| format!("create {}: {e}", path.display()))?;
    for &s in data {
        w.write_sample(s).map_err(|e| format!("write: {e}"))?;
    }
    w.finalize().map_err(|e| format!("finalize: {e}"))?;
    Ok(())
}
