//! Мини WAV I/O (через hound): чтение в интерливнутый f32 и запись f32. Движок BSRoformer выдаёт
//! WAV IEEE-float32; входной микс из ffmpeg может быть pcm16 или float — hound читает оба формата.

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

/// Интерливнутый PCM во float32 + метаданные.
pub struct Audio {
    pub sample_rate: u32,
    pub channels: u16,
    pub data: Vec<f32>,
}

/// Прочитать WAV в интерливнутый f32 (нормализуя int-семплы в [-1,1]).
pub fn read_f32(path: &std::path::Path) -> Result<Audio, String> {
    let mut r = WavReader::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let spec = r.spec();
    let data: Vec<f32> = match spec.sample_format {
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
    Ok(Audio {
        sample_rate: spec.sample_rate,
        channels: spec.channels,
        data,
    })
}

/// Записать интерливнутый f32 в WAV IEEE-float32 (тот же формат, что отдаёт движок).
pub fn write_f32(path: &std::path::Path, a: &Audio) -> Result<(), String> {
    let spec = WavSpec {
        channels: a.channels.max(1),
        sample_rate: a.sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut w = WavWriter::create(path, spec).map_err(|e| format!("create {}: {e}", path.display()))?;
    for &s in &a.data {
        w.write_sample(s).map_err(|e| format!("write: {e}"))?;
    }
    w.finalize().map_err(|e| format!("finalize: {e}"))?;
    Ok(())
}
