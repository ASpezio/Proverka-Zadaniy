//! LM Studio и Ollama «в один клик»: найти программу, установить её через winget,
//! запустить или остановить сервер, скачать модель. Ход длинных операций приходит
//! событием `local_progress` { kind, line, pct }.

use super::core::Core;
use super::json::Json;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const EXE_SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

fn hide(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// Ollama, запущенная приложением (чтобы её же и остановить).
fn ollama_child() -> &'static Mutex<Option<Child>> {
    static S: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}
/// Текущая долгая операция (установка, скачивание модели) — её pid для «Отменить».
fn busy_pid() -> &'static Mutex<Option<u32>> {
    static S: OnceLock<Mutex<Option<u32>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn env_path(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Найти программу в PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let file = format!("{}{}", name, EXE_SUFFIX);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(&file)).find(|p| p.is_file())
}

fn first_file(c: Vec<Option<PathBuf>>) -> Option<PathBuf> {
    c.into_iter().flatten().find(|p| p.is_file())
}

pub fn lms_cli() -> Option<PathBuf> {
    let home = env_path("USERPROFILE").or_else(|| env_path("HOME"));
    first_file(vec![
        home.as_ref().map(|h| h.join(".lmstudio").join("bin").join(format!("lms{}", EXE_SUFFIX))),
        home.as_ref().map(|h| h.join(".cache").join("lm-studio").join("bin").join(format!("lms{}", EXE_SUFFIX))),
        which("lms"),
    ])
}

pub fn lms_app() -> Option<PathBuf> {
    let la = env_path("LOCALAPPDATA");
    let pf = env_path("ProgramFiles");
    first_file(vec![
        la.as_ref().map(|d| d.join("Programs").join("LM Studio").join("LM Studio.exe")),
        la.as_ref().map(|d| d.join("Programs").join("lm-studio").join("LM Studio.exe")),
        pf.as_ref().map(|d| d.join("LM Studio").join("LM Studio.exe")),
    ])
}

pub fn ollama_bin() -> Option<PathBuf> {
    let la = env_path("LOCALAPPDATA");
    first_file(vec![la.as_ref().map(|d| d.join("Programs").join("Ollama").join("ollama.exe")), which("ollama")])
}

fn winget() -> Option<PathBuf> {
    first_file(vec![env_path("LOCALAPPDATA").map(|d| d.join("Microsoft").join("WindowsApps").join("winget.exe")), which("winget")])
}

fn online(core: &Core, kind: &str) -> bool {
    let url = match kind {
        "lmstudio" => "http://localhost:1234/v1/models",
        "ollama" => "http://localhost:11434/api/tags",
        _ => return false,
    };
    core.http.request("GET", url, &[], None, 2).map(|r| r.status < 500).unwrap_or(false)
}

/// Что установлено и что запущено.
pub fn status(core: &Core) -> Json {
    let lms = lms_cli();
    let app = lms_app();
    let oll = ollama_bin();
    let mut ollama_models = Json::arr();
    if online(core, "ollama") {
        if let Ok(r) = core.http.request("GET", "http://localhost:11434/api/tags", &[], None, 3) {
            if let Ok(j) = super::json::parse(&r.body) {
                for m in j.get("models").as_arr() {
                    ollama_models.push(
                        Json::obj()
                            .with("name", m.get("name").str_or(""))
                            .with("size_gb", (m.get("size").f64_or(0.0) / 1e9 * 10.0).round() / 10.0),
                    );
                }
            }
        }
    }
    let lm_on = online(core, "lmstudio");
    Json::obj()
        .with("winget", winget().is_some())
        .with("busy", busy_pid().lock().map(|g| g.is_some()).unwrap_or(false))
        .with(
            "lmstudio",
            Json::obj()
                .with("installed", lms.is_some() || app.is_some())
                .with("cli", lms.map(|p| p.to_string_lossy().to_string()))
                .with("app", app.map(|p| p.to_string_lossy().to_string()))
                .with("online", lm_on),
        )
        .with(
            "ollama",
            Json::obj()
                .with("installed", oll.is_some())
                .with("bin", oll.map(|p| p.to_string_lossy().to_string()))
                .with("online", online(core, "ollama"))
                .with("ours", ollama_child().lock().map(|g| g.is_some()).unwrap_or(false))
                .with("models", ollama_models),
        )
}

/// Убрать из строки вывода цветовые коды терминала и спиннеры.
pub fn clean_line(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                while let Some(&n) = it.peek() {
                    it.next();
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if ('\u{2800}'..='\u{28ff}').contains(&c) || c == '\u{8}' {
            continue; // символы-спиннеры Брайля, backspace
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// Последний процент в строке («pulling 4f2a…  45% ▕██  ▏ 2.1 GB/4.7 GB»).
pub fn pct_of(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut best = None;
    for (i, ch) in s.char_indices() {
        if ch != '%' {
            continue;
        }
        let mut j = i;
        while j > 0 && (b[j - 1].is_ascii_digit() || b[j - 1] == b'.' || b[j - 1] == b',') {
            j -= 1;
        }
        if j < i {
            if let Ok(v) = s[j..i].replace(',', ".").parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    best = Some(v);
                }
            }
        }
    }
    best
}

/// Запустить программу, показывая ход по строкам; дождаться конца.
fn run_progress(core: &Core, kind: &str, exe: &PathBuf, args: &[&str], limit: Duration) -> Result<String, String> {
    if busy_pid().lock().map(|g| g.is_some()).unwrap_or(false) {
        return Err("Уже идёт установка или скачивание — дождитесь окончания".into());
    }
    let mut c = Command::new(exe);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide(&mut c);
    let mut child = c.spawn().map_err(|e| format!("не удалось запустить {}: {}", exe.display(), e))?;
    *busy_pid().lock().unwrap_or_else(|e| e.into_inner()) = Some(child.id());
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let mut readers = Vec::new();
    for mut src in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut cur: Vec<u8> = Vec::new();
            loop {
                match src.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for &b in &buf[..n] {
                            if b == b'\n' || b == b'\r' {
                                if !cur.is_empty() {
                                    let _ = tx.send(String::from_utf8_lossy(&cur).to_string());
                                    cur.clear();
                                }
                            } else {
                                cur.push(b);
                            }
                        }
                    }
                }
            }
            if !cur.is_empty() {
                let _ = tx.send(String::from_utf8_lossy(&cur).to_string());
            }
        }));
    }
    drop(tx);
    let start = Instant::now();
    let mut last = String::new();
    let mut tail: Vec<String> = Vec::new();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let status = loop {
        while let Ok(l) = rx.try_recv() {
            let l = clean_line(&l);
            if l.is_empty() || l == last {
                continue;
            }
            last = l.clone();
            tail.push(l.clone());
            if tail.len() > 6 {
                tail.remove(0);
            }
            if last_emit.elapsed() >= Duration::from_millis(250) {
                last_emit = Instant::now();
                (core.emit)("local_progress", Json::obj().with("kind", kind).with("line", l.as_str()).with("pct", pct_of(&l)).to_string());
            }
        }
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(120));
    };
    for r in readers {
        let _ = r.join();
    }
    while let Ok(l) = rx.try_recv() {
        let l = clean_line(&l);
        if !l.is_empty() {
            tail.push(l);
        }
    }
    *busy_pid().lock().unwrap_or_else(|e| e.into_inner()) = None;
    (core.emit)("local_progress", Json::obj().with("kind", kind).with("line", "").with("pct", Json::Null).with("done", true).to_string());
    match status {
        Some(s) if s.success() => Ok(tail.last().cloned().unwrap_or_default()),
        Some(_) => Err(format!("не получилось: {}", tail.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join(" • "))),
        None => Err("операция прервана".into()),
    }
}

/// Прервать установку или скачивание.
pub fn cancel() -> bool {
    let pid = busy_pid().lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(pid) = pid else { return false };
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        hide(&mut c);
        let _ = c.status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    true
}

/// Установить программу через winget (встроен в Windows 10/11).
pub fn install(core: &Core, kind: &str) -> Result<String, String> {
    let id = match kind {
        "lmstudio" => "ElementLabs.LMStudio",
        "ollama" => "Ollama.Ollama",
        _ => return Err("неизвестная программа".into()),
    };
    let w = winget().ok_or("В Windows нет winget (Установщик приложений). Скачайте программу с сайта — кнопка «Открыть сайт».")?;
    core.log(&format!("Установка {} через winget", id));
    run_progress(
        core,
        kind,
        &w,
        &["install", "-e", "--id", id, "--silent", "--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"],
        Duration::from_secs(1800),
    )?;
    let name = if kind == "ollama" { "Ollama" } else { "LM Studio" };
    super::notify::send(core, "long", &format!("{} установлен", name), "Можно запускать сервер и скачивать модели.");
    Ok(format!("{} установлен", name))
}

fn wait_online(core: &Core, kind: &str, secs: u64) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if online(core, kind) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Запустить сервер.
pub fn start(core: &Core, kind: &str) -> Result<String, String> {
    if online(core, kind) {
        return Ok("Сервер уже работает".into());
    }
    match kind {
        "lmstudio" => {
            if let Some(lms) = lms_cli() {
                let mut c = Command::new(&lms);
                c.args(["server", "start"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                hide(&mut c);
                let _ = c.status();
                if wait_online(core, kind, 25) {
                    return Ok("Сервер LM Studio запущен".into());
                }
            }
            if let Some(app) = lms_app() {
                let _ = Command::new(&app).spawn();
                if wait_online(core, kind, 30) {
                    return Ok("LM Studio открыт, сервер работает".into());
                }
                return Err("LM Studio открыт, но сервер не включён: в LM Studio откройте вкладку «Developer» и включите «Start server» (или запустите LM Studio один раз — тогда приложение сможет включать сервер само)".into());
            }
            Err("LM Studio не найден — нажмите «Установить»".into())
        }
        "ollama" => {
            let bin = ollama_bin().ok_or("Ollama не найдена — нажмите «Установить»")?;
            let mut c = Command::new(&bin);
            c.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            hide(&mut c);
            let child = c.spawn().map_err(|e| format!("не удалось запустить Ollama: {}", e))?;
            *ollama_child().lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            if wait_online(core, kind, 20) {
                Ok("Сервер Ollama запущен".into())
            } else {
                Err("Ollama запущена, но не отвечает — подождите несколько секунд и нажмите «Обновить»".into())
            }
        }
        _ => Err("неизвестный сервер".into()),
    }
}

/// Остановить сервер.
pub fn stop(core: &Core, kind: &str) -> Result<String, String> {
    match kind {
        "lmstudio" => {
            let lms = lms_cli().ok_or("Остановите сервер в самой LM Studio (вкладка «Developer»)")?;
            let mut c = Command::new(&lms);
            c.args(["server", "stop"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            hide(&mut c);
            let _ = c.status();
            Ok("Сервер LM Studio остановлен".into())
        }
        "ollama" => {
            if let Some(mut ch) = ollama_child().lock().unwrap_or_else(|e| e.into_inner()).take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
            #[cfg(windows)]
            if online(core, kind) {
                for im in ["ollama app.exe", "ollama.exe"] {
                    let mut c = Command::new("taskkill");
                    c.args(["/IM", im, "/F"]);
                    hide(&mut c);
                    let _ = c.status();
                }
            }
            std::thread::sleep(Duration::from_millis(400));
            if online(core, kind) {
                Err("Ollama продолжает работать — закройте её значок у часов".into())
            } else {
                Ok("Сервер Ollama остановлен".into())
            }
        }
        _ => Err("неизвестный сервер".into()),
    }
}

/// Проверка названия модели: буквы, цифры, «:/._-@».
pub fn valid_model(name: &str) -> bool {
    !name.is_empty() && name.len() < 200 && !name.starts_with('-') && name.chars().all(|c| c.is_ascii_alphanumeric() || ":/._-@".contains(c))
}

/// Скачать модель: `ollama pull` или `lms get`.
pub fn pull(core: &Core, kind: &str, model: &str) -> Result<String, String> {
    let model = model.trim();
    if !valid_model(model) {
        return Err("Название модели — латиница, цифры и знаки : / . _ - (например qwen2.5:7b)".into());
    }
    match kind {
        "ollama" => {
            let bin = ollama_bin().ok_or("Ollama не найдена — нажмите «Установить»")?;
            if !online(core, kind) {
                start(core, kind)?;
            }
            run_progress(core, kind, &bin, &["pull", model], Duration::from_secs(4 * 3600))?;
            super::notify::send(core, "long", "Модель скачана", &format!("{} готова к работе в Ollama.", model));
            Ok(format!("Модель {} скачана", model))
        }
        "lmstudio" => {
            let lms = lms_cli().ok_or("Не найдена командная строка LM Studio (lms): запустите LM Studio хотя бы один раз")?;
            run_progress(core, kind, &lms, &["get", model, "--yes"], Duration::from_secs(4 * 3600))?;
            super::notify::send(core, "long", "Модель скачана", &format!("{} готова к работе в LM Studio.", model));
            Ok(format!("Модель {} скачана", model))
        }
        _ => Err("неизвестный сервер".into()),
    }
}

/// Убедиться, что сервер есть среди нейросетей приложения, и (по желанию) выбрать его.
pub fn ensure_provider(core: &Core, kind: &str, model: Option<&str>, activate: bool) -> String {
    let url = if kind == "ollama" { "http://localhost:11434" } else { "http://localhost:1234/v1" };
    let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
    let mut provs: Vec<Json> = st.get("providers").as_arr().to_vec();
    let idx = provs.iter().position(|p| p.get("kind").str_or("") == kind && p.get("base_url").str_or("").trim_end_matches('/') == url);
    let id = match idx {
        Some(i) => {
            if let Some(m) = model {
                provs[i].set("model", m);
            }
            provs[i].get("id").str_or("")
        }
        None => {
            let id = kind.to_string();
            let id = if provs.iter().any(|p| p.get("id").str_or("") == id) { format!("{}-{}", kind, provs.len() + 1) } else { id };
            provs.push(
                Json::obj()
                    .with("id", id.as_str())
                    .with("kind", kind)
                    .with("name", if kind == "ollama" { "Ollama" } else { "LM Studio" })
                    .with("base_url", url)
                    .with("model", model.unwrap_or(""))
                    .with("temperature", 0.1)
                    .with("max_tokens", 3000)
                    .with("num_ctx", 16384)
                    .with("structured", true)
                    .with("timeout_s", 900)
                    .with("local", true),
            );
            id
        }
    };
    st.set("providers", Json::Arr(provs));
    if activate {
        st.set("active_provider", id.as_str());
    }
    drop(st);
    core.save_settings();
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_lines() {
        assert_eq!(clean_line("\u{1b}[?25l\u{280b} pulling manifest \u{1b}[K"), "pulling manifest");
        assert_eq!(pct_of("pulling 4f2a:  45% ▕██  ▏ 2.1 GB/4.7 GB"), Some(45.0));
        assert_eq!(pct_of("Downloading 12.5% of 3 files, 100% verified? no: 7%"), Some(7.0));
        assert_eq!(pct_of("no percent"), None);
        assert!(valid_model("qwen2.5:7b"));
        assert!(valid_model("lmstudio-community/Qwen2.5-7B-Instruct-GGUF"));
        assert!(!valid_model("-rf"));
        assert!(!valid_model("a b"));
        assert!(!valid_model("x&calc"));
    }
}
