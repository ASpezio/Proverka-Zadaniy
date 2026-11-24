//! Обновление приложения из релизов GitHub (ASpezio/Proverka-Zadaniy):
//! проверить последнюю версию, скачать новый ProverkaZadaniy.exe и заменить им
//! установленный — после выхода из приложения его подменяет маленький скрипт PowerShell
//! и снова запускает приложение.

use super::core::{now_s, Core};
use super::json::{self, Json};

pub const REPO: &str = "ASpezio/Proverka-Zadaniy";
pub const EXE_ASSET: &str = "ProverkaZadaniy.exe";

/// «1.10.2» > «1.9.9»; «v» в начале и хвосты вроде «-beta» не мешают.
pub fn newer(remote: &str, local: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim().trim_start_matches(['v', 'V']).split(|c: char| c == '.' || c == '-' || c == '+').take(3).map(|x| x.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)).collect()
    };
    let (mut a, mut b) = (parse(remote), parse(local));
    a.resize(3, 0);
    b.resize(3, 0);
    a > b
}

/// Нужно ли проверять при запуске: режим start — всегда, weekly — раз в 7 дней.
pub fn due(settings: &Json) -> bool {
    let u = settings.get("updates");
    match u.get("mode").str_or("start").as_str() {
        "start" => true,
        "weekly" => now_s().saturating_sub(u.get("last_check").f64_or(0.0) as u64) >= 7 * 86400,
        _ => false,
    }
}

/// Последний релиз на GitHub.
pub fn check(core: &Core) -> Result<Json, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let h = vec![("User-Agent".to_string(), "proverka-zadaniy".to_string()), ("Accept".to_string(), "application/vnd.github+json".to_string())];
    let r = core.http.request("GET", &url, &h, None, 20).map_err(|e| format!("Не удалось связаться с GitHub: {}", e))?;
    if r.status == 404 {
        return Ok(Json::obj().with("available", false).with("current", env!("CARGO_PKG_VERSION")).with("message", "Опубликованных версий пока нет"));
    }
    if r.status >= 400 {
        return Err(format!("GitHub ответил ошибкой {}", r.status));
    }
    let j = json::parse(&r.body).map_err(|_| "Непонятный ответ GitHub".to_string())?;
    {
        let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
        let mut u = st.get("updates").clone();
        if u.is_null() {
            u = Json::obj();
        }
        u.set("last_check", now_s());
        st.set("updates", u);
    }
    core.save_settings();
    let tag = j.get("tag_name").str_or("");
    let cur = env!("CARGO_PKG_VERSION");
    let assets = j.get("assets").as_arr();
    let exe = assets.iter().find(|a| a.get("name").str_or("") == EXE_ASSET);
    let setup = assets.iter().find(|a| {
        let n = a.get("name").str_or("").to_lowercase();
        n.ends_with(".exe") && n != EXE_ASSET.to_lowercase()
    });
    Ok(Json::obj()
        .with("available", newer(&tag, cur) && exe.is_some())
        .with("current", cur)
        .with("version", tag.trim_start_matches(['v', 'V']))
        .with("name", j.get("name").str_or(&tag))
        .with("notes", j.get("body").str_or(""))
        .with("published", j.get("published_at").str_or(""))
        .with("page", j.get("html_url").str_or(""))
        .with("url", exe.map(|a| a.get("browser_download_url").str_or("")).unwrap_or_default())
        .with("size", exe.map(|a| a.get("size").f64_or(0.0)).unwrap_or(0.0))
        .with("setup_url", setup.map(|a| a.get("browser_download_url").str_or("")).unwrap_or_default()))
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Скачать новую версию и подготовить замену. Возвращает путь скрипта, который
/// нужно запустить сразу перед выходом из приложения.
pub fn download(core: &Core, url: &str, version: &str) -> Result<String, String> {
    if !url.starts_with("https://github.com/") && !url.starts_with("https://objects.githubusercontent.com/") {
        return Err("Обновление скачивается только с GitHub".into());
    }
    let dir = core.data_dir.join("updates");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let new_exe = dir.join(EXE_ASSET);
    let last = std::sync::Mutex::new(std::time::Instant::now());
    core.http.download(url, &new_exe, &|got, total| {
        let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
        if l.elapsed().as_millis() > 250 || got == total {
            *l = std::time::Instant::now();
            (core.emit)("update_progress", Json::obj().with("got", got).with("total", total).to_string());
        }
    })?;
    let size = std::fs::metadata(&new_exe).map(|m| m.len()).unwrap_or(0);
    if size < 500_000 {
        return Err("Скачанный файл повреждён — попробуйте ещё раз".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let pid = std::process::id();
    // скрипт: дождаться выхода, заменить exe (старый — в .old на случай отката), обновить версию в «Приложениях», запустить
    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'\r\n\
         $exe = {exe}\r\n$new = {new}\r\n\
         for ($i = 0; $i -lt 120 -and (Get-Process -Id {pid} -ErrorAction SilentlyContinue); $i++) {{ Start-Sleep -Milliseconds 500 }}\r\n\
         Start-Sleep -Milliseconds 400\r\n\
         Copy-Item -LiteralPath $exe -Destination ($exe + '.old') -Force\r\n\
         $ok = $false\r\n\
         for ($i = 0; $i -lt 20 -and -not $ok; $i++) {{ try {{ Copy-Item -LiteralPath $new -Destination $exe -Force -ErrorAction Stop; $ok = $true }} catch {{ Start-Sleep -Milliseconds 500 }} }}\r\n\
         if ($ok) {{\r\n  $k = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\ProverkaZadaniy'\r\n  if (Test-Path $k) {{ Set-ItemProperty -Path $k -Name DisplayVersion -Value {ver} }}\r\n}}\r\n\
         Start-Process -FilePath $exe\r\n",
        exe = ps_quote(&exe.to_string_lossy()),
        new = ps_quote(&new_exe.to_string_lossy()),
        pid = pid,
        ver = ps_quote(version)
    );
    let sp = dir.join("apply.ps1");
    // UTF-8 с BOM — иначе PowerShell 5 исказит русские пути
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&sp, bytes).map_err(|e| e.to_string())?;
    Ok(sp.to_string_lossy().to_string())
}

/// Запустить скрипт замены и выйти из приложения.
pub fn apply_and_exit(core: &Core, script: &str) -> Result<(), String> {
    if !std::path::Path::new(script).is_file() {
        return Err("Сначала скачайте обновление".into());
    }
    core.save_session();
    core.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
    core.save_settings();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File", script])
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("не удалось запустить обновление: {}", e))?;
    }
    #[cfg(not(windows))]
    {
        return Err("Обновление поддерживается только в Windows".into());
    }
    #[cfg(windows)]
    {
        super::llamacpp::stop_all();
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            std::process::exit(0);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert!(newer("v1.1.0", "1.0.0"));
        assert!(newer("1.10.0", "1.9.9"));
        assert!(!newer("v1.0.0", "1.0.0"));
        assert!(!newer("0.9", "1.0.0"));
        assert!(newer("2", "1.9.9"));
        assert!(!newer("garbage", "1.0.0"));
        let s = super::super::json::parse(r#"{"updates":{"mode":"weekly","last_check":0}}"#).unwrap();
        assert!(due(&s));
        let off = super::super::json::parse(r#"{"updates":{"mode":"manual"}}"#).unwrap();
        assert!(!due(&off));
    }
}
