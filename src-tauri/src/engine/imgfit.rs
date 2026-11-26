//! Уменьшение больших фотографий перед отправкой нейросети.
//!
//! Фото с телефона (4000×3000 и больше) многие модели (Qwen-VL и др.) превращают в тысячи
//! токенов: распознавание идёт долго, а почерк читается не лучше, чем на копии 1600 px.
//! Копия делается один раз и хранится рядом со страницами сканов (`pages/`), поэтому
//! исчезает вместе с работой. Уменьшает:
//! * Windows — встроенный System.Drawing через PowerShell (ничего ставить не нужно);
//!   заодно поворачивает фото по EXIF, чтобы модель не читала текст боком;
//! * другие системы — ImageMagick (`convert`), если он установлен.
//! Не получилось — уходит исходное изображение, как раньше.

use super::core::{fnv, Core};
use std::path::Path;
use std::time::{Duration, Instant};

/// Длинная сторона по умолчанию, px. 0 в настройках — не уменьшать.
pub const DEFAULT_MAX_SIDE: u32 = 1600;

pub fn max_side(core: &Core) -> u32 {
    core.settings().get("media").get("max_side").i64_or(DEFAULT_MAX_SIDE as i64).clamp(0, 10000) as u32
}

/// Ширина и высота по заголовку файла, без распаковки: JPEG, PNG, GIF, BMP, WebP.
pub fn dims(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]) as u32;
    let le16 = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as u32;
    if b.len() >= 24 && b.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
        return Some((w, h));
    }
    if b.len() >= 10 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return Some((le16(6), le16(8)));
    }
    if b.len() >= 26 && b.starts_with(b"BM") {
        let w = i32::from_le_bytes([b[18], b[19], b[20], b[21]]);
        let h = i32::from_le_bytes([b[22], b[23], b[24], b[25]]);
        return Some((w.unsigned_abs(), h.unsigned_abs()));
    }
    if b.len() >= 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let le24 = |i: usize| b[i] as u32 | (b[i + 1] as u32) << 8 | (b[i + 2] as u32) << 16;
        return match &b[12..16] {
            b"VP8X" => Some((1 + le24(24), 1 + le24(27))),
            b"VP8 " => Some((le16(26) & 0x3fff, le16(28) & 0x3fff)),
            b"VP8L" => {
                let w = 1 + (b[21] as u32 | (b[22] as u32 & 0x3f) << 8);
                let h = 1 + (b[22] as u32 >> 6 | (b[23] as u32) << 2 | (b[24] as u32 & 0x0f) << 10);
                Some((w, h))
            }
            _ => None,
        };
    }
    if b.len() >= 4 && b[0] == 0xFF && b[1] == 0xD8 {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            if m == 0xFF {
                i += 1;
                continue;
            }
            if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
                i += 2;
                continue;
            }
            // SOF0…SOF15 (кроме DHT, JPG, DAC): высота, затем ширина
            if (0xC0..=0xCF).contains(&m) && m != 0xC4 && m != 0xC8 && m != 0xCC {
                return Some((be16(i + 7), be16(i + 5)));
            }
            if m == 0xDA || m == 0xD9 {
                return None;
            }
            i += 2 + be16(i + 2) as usize;
        }
    }
    None
}

/// Изображение для нейросети: если длинная сторона больше настройки — уменьшенная копия
/// (PNG остаётся PNG — скриншоты без потерь, остальное — JPEG). Иначе — как есть.
/// Возвращает (mime, байты).
pub fn fit(core: &Core, mime: &str, bytes: Vec<u8>) -> (String, Vec<u8>) {
    let max = max_side(core);
    let Some((w, h)) = dims(&bytes) else { return (mime.to_string(), bytes) };
    if max == 0 || w.max(h) <= max {
        return (mime.to_string(), bytes);
    }
    let png = bytes.starts_with(b"\x89PNG");
    let ext = if png { "png" } else { "jpg" };
    let out_mime = if png { "image/png" } else { "image/jpeg" };
    // имя начинается с хэша исходника — копия живёт, пока жива страница работы
    let dir = core.data_dir.join("pages");
    let _ = std::fs::create_dir_all(&dir);
    let key = format!("{:016x}_s{}", fnv(&bytes), max);
    let out = dir.join(format!("{}.{}", key, ext));
    if let Ok(b) = std::fs::read(&out) {
        if dims(&b).map(|(a, c)| a.max(c) <= max).unwrap_or(false) {
            return (out_mime.to_string(), b);
        }
    }
    let src_ext = match mime {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let src = dir.join(format!("{}_src.{}", key, src_ext));
    if std::fs::write(&src, &bytes).is_err() {
        return (mime.to_string(), bytes);
    }
    let r = shrink_file(&core.data_dir, &src, &out, max, png);
    let _ = std::fs::remove_file(&src);
    match r.and_then(|_| std::fs::read(&out).map_err(|e| e.to_string())) {
        Ok(b) if dims(&b).map(|(a, c)| a.max(c) <= max && a > 0 && c > 0).unwrap_or(false) => {
            core.log(&format!("Изображение {}×{} уменьшено до {} px по длинной стороне ({} → {} КБ)", w, h, max, bytes.len() / 1024, b.len() / 1024));
            (out_mime.to_string(), b)
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&out);
            (mime.to_string(), bytes)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            core.log(&format!("Изображение {}×{} отправлено без уменьшения: {}", w, h, e));
            (mime.to_string(), bytes)
        }
    }
}

/// То же для base64 (фото из буфера обмена, задание с фото).
pub fn fit_b64(core: &Core, mime: &str, b64: &str) -> (String, String) {
    let bytes = super::api::b64_decode(b64);
    let Some((w, h)) = dims(&bytes) else { return (mime.to_string(), b64.to_string()) };
    let max = max_side(core);
    if max == 0 || w.max(h) <= max {
        return (mime.to_string(), b64.to_string());
    }
    let (m, b) = fit(core, mime, bytes);
    (m, super::api::b64(&b))
}

/// Скрипт PowerShell (только ASCII — Windows PowerShell 5.1 читает .ps1 без BOM как ANSI).
/// Пути и параметры — через переменные окружения: так не мешают пробелы и кириллица.
#[cfg_attr(not(windows), allow(dead_code))]
const SHRINK_PS1: &str = r#"$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$max = [int]$env:PV_MAX
$img = [System.Drawing.Image]::FromFile($env:PV_SRC)
try {
  if ($img.PropertyIdList -contains 274) {
    $o = [int][BitConverter]::ToUInt16($img.GetPropertyItem(274).Value, 0)
    $f = @{2='RotateNoneFlipX';3='Rotate180FlipNone';4='Rotate180FlipX';5='Rotate90FlipX';6='Rotate90FlipNone';7='Rotate270FlipX';8='Rotate270FlipNone'}[$o]
    if ($f) { $img.RotateFlip([System.Drawing.RotateFlipType]$f) }
  }
  $k = [Math]::Min(1.0, $max / [double][Math]::Max($img.Width, $img.Height))
  $w = [Math]::Max(1, [int][Math]::Floor($img.Width * $k))
  $h = [Math]::Max(1, [int][Math]::Floor($img.Height * $k))
  $bmp = New-Object System.Drawing.Bitmap($w, $h)
  try {
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.Clear([System.Drawing.Color]::White)
    $g.DrawImage($img, 0, 0, $w, $h)
    $g.Dispose()
    if ($env:PV_FMT -eq 'png') {
      $bmp.Save($env:PV_DST, [System.Drawing.Imaging.ImageFormat]::Png)
    } else {
      $enc = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' } | Select-Object -First 1
      $ps = New-Object System.Drawing.Imaging.EncoderParameters(1)
      $ps.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter([System.Drawing.Imaging.Encoder]::Quality, [long]88)
      $bmp.Save($env:PV_DST, $enc, $ps)
    }
  } finally { $bmp.Dispose() }
} finally { $img.Dispose() }
"#;

fn shrink_file(data_dir: &Path, src: &Path, dst: &Path, max: u32, png: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script = data_dir.join("pages").join("_shrink.ps1");
        let _ = std::fs::create_dir_all(data_dir.join("pages"));
        if std::fs::read_to_string(&script).map(|t| t != SHRINK_PS1).unwrap_or(true) {
            std::fs::write(&script, SHRINK_PS1).map_err(|e| e.to_string())?;
        }
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(&script)
            .env("PV_SRC", src)
            .env("PV_DST", dst)
            .env("PV_MAX", max.to_string())
            .env("PV_FMT", if png { "png" } else { "jpg" })
            .creation_flags(0x08000000);
        return run_with_timeout(cmd, 60);
    }
    #[cfg(not(windows))]
    {
        let _ = (data_dir, png);
        let mut cmd = std::process::Command::new("convert");
        cmd.arg(src).args(["-auto-orient", "-resize", &format!("{}x{}>", max, max), "-quality", "88"]).arg(dst);
        run_with_timeout(cmd, 60)
    }
}

fn run_with_timeout(mut cmd: std::process::Command, secs: u64) -> Result<(), String> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("не удалось запустить уменьшение: {}", e))?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) if st.success() => return Ok(()),
            Ok(Some(st)) => {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    use std::io::Read;
                    let _ = e.read_to_string(&mut err);
                }
                return Err(format!("код {:?}: {}", st.code(), err.trim().chars().take(300).collect::<String>()));
            }
            Ok(None) if t0.elapsed() > Duration::from_secs(secs) => {
                let _ = child.kill();
                return Err("уменьшение заняло слишком долго".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_from_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&4000u32.to_be_bytes());
        png.extend_from_slice(&3000u32.to_be_bytes());
        assert_eq!(dims(&png), Some((4000, 3000)));
        // JPEG: APP0, затем SOF0 (высота 3024, ширина 4032)
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        jpg.extend_from_slice(&3024u16.to_be_bytes());
        jpg.extend_from_slice(&4032u16.to_be_bytes());
        jpg.extend_from_slice(&[0x03, 0, 0, 0, 0, 0, 0]);
        assert_eq!(dims(&jpg), Some((4032, 3024)));
        let gif = b"GIF89a\x40\x01\xF0\x00".to_vec();
        assert_eq!(dims(&gif), Some((320, 240)));
        assert_eq!(dims(b"not an image at all, really"), None);
    }

    /// Настоящее уменьшение: в Windows — PowerShell, в других системах — ImageMagick, если есть.
    #[test]
    fn shrink_real_image() {
        let dir = std::env::temp_dir().join(format!("pv_imgfit_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let src = dir.join("big.bmp");
        // BMP 2400×1200, 24 бита — простейший формат, который читают все
        let (w, h) = (2400u32, 1200u32);
        let row = ((w * 3 + 3) / 4) * 4;
        let size = 54 + row * h;
        let mut b = Vec::with_capacity(size as usize);
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
        b.extend_from_slice(&(w as i32).to_le_bytes());
        b.extend_from_slice(&(h as i32).to_le_bytes());
        b.extend_from_slice(&[1, 0, 24, 0, 0, 0, 0, 0]);
        b.extend_from_slice(&(row * h).to_le_bytes());
        b.extend_from_slice(&[0; 16]);
        for y in 0..h {
            for x in 0..w {
                b.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128]);
            }
            b.resize(54 + ((y + 1) * row) as usize, 0);
        }
        std::fs::write(&src, &b).unwrap();
        let dst = dir.join("small.jpg");
        match shrink_file(&dir, &src, &dst, 1600, false) {
            Ok(()) => {
                let out = std::fs::read(&dst).unwrap();
                assert_eq!(dims(&out), Some((1600, 800)));
            }
            // без ImageMagick в Linux уменьшать нечем — в Windows ошибка недопустима
            Err(e) => assert!(!cfg!(windows), "уменьшение не сработало: {}", e),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
