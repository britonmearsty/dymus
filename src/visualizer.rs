use serde_json::Value;

#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub bands: Vec<f32>,
    pub waveform: Vec<f32>,
    pub scope: Vec<(f32, f32)>,
    pub rms: f32,
}

const FFT_SIZE: usize = 2048;
const BAND_COUNT: usize = 48;
const WAVEFORM_POINTS: usize = 256;
const SCOPE_POINTS: usize = 192;
const SAMPLE_RATE: f32 = 48_000.0;

pub fn analyze(interleaved: &[f32], channels: usize) -> AudioFrame {
    let channels = channels.max(1);
    let frames = interleaved.len() / channels;
    if frames == 0 {
        return AudioFrame {
            bands: vec![0.0; BAND_COUNT],
            waveform: vec![0.0; WAVEFORM_POINTS],
            scope: vec![(0.0, 0.0); SCOPE_POINTS],
            rms: 0.0,
        };
    }
    let left = |frame: usize| interleaved[frame * channels].clamp(-1.0, 1.0);
    let right = |frame: usize| {
        if channels > 1 {
            interleaved[frame * channels + 1].clamp(-1.0, 1.0)
        } else {
            left(frame)
        }
    };
    let rms = ((0..frames)
        .map(|frame| (left(frame) * left(frame) + right(frame) * right(frame)) * 0.5)
        .sum::<f32>()
        / frames as f32)
        .sqrt();
    let waveform = (0..WAVEFORM_POINTS)
        .map(|point| left(point * frames / WAVEFORM_POINTS))
        .collect();
    let scope = (0..SCOPE_POINTS)
        .map(|point| {
            let frame = point * frames / SCOPE_POINTS;
            (left(frame), right(frame))
        })
        .collect();

    let mut spectrum = vec![(0.0f32, 0.0f32); FFT_SIZE];
    for (index, value) in spectrum.iter_mut().enumerate() {
        let sample = if index < frames {
            (left(index) + right(index)) * 0.5
        } else {
            0.0
        };
        let window = 0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / FFT_SIZE as f32).cos();
        value.0 = sample * window;
    }
    fft(&mut spectrum);
    let nyquist_bin = FFT_SIZE / 2;
    let min_frequency = 35.0f32;
    let max_frequency = 18_000.0f32;
    let mut bands = Vec::with_capacity(BAND_COUNT);
    for band in 0..BAND_COUNT {
        let low =
            min_frequency * (max_frequency / min_frequency).powf(band as f32 / BAND_COUNT as f32);
        let high = min_frequency
            * (max_frequency / min_frequency).powf((band + 1) as f32 / BAND_COUNT as f32);
        let start = ((low * FFT_SIZE as f32 / SAMPLE_RATE).floor() as usize).max(1);
        let end = ((high * FFT_SIZE as f32 / SAMPLE_RATE).ceil() as usize).min(nyquist_bin);
        let magnitude = spectrum[start..end.max(start + 1).min(nyquist_bin)]
            .iter()
            .map(|(re, im)| re.hypot(*im) * 2.0 / FFT_SIZE as f32)
            .fold(0.0f32, f32::max);
        let db = 20.0 * magnitude.max(1e-6).log10();
        bands.push(((db + 62.0) / 56.0).clamp(0.0, 1.0));
    }
    AudioFrame {
        bands,
        waveform,
        scope,
        rms,
    }
}

fn fft(values: &mut [(f32, f32)]) {
    let n = values.len();
    let mut reversed = 0usize;
    for index in 1..n {
        let mut bit = n >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            values.swap(index, reversed);
        }
    }
    let mut size = 2;
    while size <= n {
        let angle = -std::f32::consts::TAU / size as f32;
        let (sin, cos) = angle.sin_cos();
        for chunk in values.chunks_exact_mut(size) {
            let (mut wr, mut wi) = (1.0f32, 0.0f32);
            for offset in 0..size / 2 {
                let even = chunk[offset];
                let odd = chunk[offset + size / 2];
                let rotated = (odd.0 * wr - odd.1 * wi, odd.0 * wi + odd.1 * wr);
                chunk[offset] = (even.0 + rotated.0, even.1 + rotated.1);
                chunk[offset + size / 2] = (even.0 - rotated.0, even.1 - rotated.1);
                (wr, wi) = (wr * cos - wi * sin, wr * sin + wi * cos);
            }
        }
        size <<= 1;
    }
}

pub fn find_dymus_node(dump: &[u8]) -> Option<String> {
    let objects: Value = serde_json::from_slice(dump).ok()?;
    objects.as_array()?.iter().find_map(|object| {
        let props = object.get("info")?.get("props")?;
        let application = props
            .get("application.name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let class = props
            .get("media.class")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if application.eq_ignore_ascii_case("Dymus")
            && (class.is_empty() || class == "Stream/Output/Audio")
        {
            props
                .get("object.serial")
                .and_then(|serial| {
                    serial
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| serial.as_u64().map(|number| number.to_string()))
                })
                .or_else(|| {
                    props
                        .get("node.name")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_the_dymus_playback_node() {
        let dump = br#"[
          {"id": 7, "type": "PipeWire:Interface:Node", "info": {"props": {"object.serial": "107", "node.name": "other", "application.name": "Other", "media.class": "Stream/Output/Audio"}}},
          {"id": 42, "type": "PipeWire:Interface:Node", "info": {"props": {"object.serial": "1042", "node.name": "dymus-out", "application.name": "Dymus", "media.class": "Stream/Output/Audio"}}}
        ]"#;
        assert_eq!(find_dymus_node(dump).as_deref(), Some("1042"));
    }

    #[test]
    fn analyzes_tone_into_audio_bands_and_waveform() {
        let samples: Vec<f32> = (0..FFT_SIZE * 2)
            .flat_map(|frame| {
                let sample = (std::f32::consts::TAU * 440.0 * frame as f32 / SAMPLE_RATE).sin();
                [sample, sample]
            })
            .collect();
        let analyzed = analyze(&samples, 2);
        assert_eq!(analyzed.bands.len(), BAND_COUNT);
        assert_eq!(analyzed.waveform.len(), WAVEFORM_POINTS);
        assert!(analyzed.rms > 0.6);
        assert!(analyzed.bands.iter().copied().fold(0.0f32, f32::max) > 0.7);
    }

    #[test]
    fn silence_has_no_rms_or_spectrum() {
        let analyzed = analyze(&vec![0.0; FFT_SIZE * 2], 2);
        assert_eq!(analyzed.rms, 0.0);
        assert!(analyzed.bands.iter().all(|value| *value == 0.0));
    }
}
