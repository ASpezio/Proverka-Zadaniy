//! llama.cpp «в один клик»: приложение само скачивает llama-server (если его нет),
//! запускает по серверу на каждую выбранную модель .gguf и подключает их к проверке.
//! От преподавателя нужно только выбрать папку с моделями, контекст и сами модели.

use super::core::Core;
use super::json::{self, Json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const EXE: &str = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };
const PORT_BASE: u16 = 8090;
pub const AUTO_PREFIX: &str = "llamacpp-auto";

pub struct Running {
    pub model: String,
    pub path: String,
    pub port: u16,
    pub ctx: i64,
    pub child: Child,
    pub log: PathBuf,
}

fn servers() -> &'static Mutex<Vec<Running>> {
    static S: OnceLock<Mutex<Vec<Running>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

/// Остановить все запущенные приложением серверы (в том числе при выходе).
pub fn stop_all() -> usize {
    let mut list = servers().lock().unwrap_or_else(|e| e.into_inner());
    let n = list.len();
    for r in list.iter_mut() {
        let _ = r.child.kill();
        let _ = r.child.wait();
    }
    list.clear();
    n
}

fn home_dir(core: &Core) -> PathBuf {
    core.data_dir.join("llama.cpp")
}

fn find_in(dir: &Path, depth: usize) -> Option<PathBuf> {
    let p = dir.join(EXE);
    if p.is_file() {
        return Some(p);
    }
    if depth == 0 {
        return None;
    }
    let rd = std::fs::read_dir(dir).ok()?;
    for e in rd.filter_map(|e| e.ok()) {
        if e.path().is_dir() {
            if let Some(x) = find_in(&e.path(), depth - 1) {
                return Some(x);
            }
        }
    }
    None
}

/// Где лежит llama-server: указанная папка → скачанная приложением → PATH.
pub fn find_bin(core: &Core) -> Option<PathBuf> {
    let st = core.settings();
    let dir = st.get("llama").get("bin_dir").str_or("");
    if !dir.is_empty() {
        if let Some(p) = find_in(Path::new(&dir), 2) {
            return Some(p);
        }
    }
    if let Some(p) = find_in(&home_dir(core), 2) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(EXE)).find(|p| p.is_file())
}

fn no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
    let _ = c;
}

/// Видеокарты NVIDIA: (имя, всего МБ, занято МБ).
pub fn gpus() -> Vec<(String, i64, i64)> {
    let mut c = Command::new("nvidia-smi");
    c.args(["--query-gpu=name,memory.total,memory.used", "--format=csv,noheader,nounits"]);
    no_window(&mut c);
    let Ok(o) = c.output() else { return Vec::new() };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split(',').map(|x| x.trim()).collect();
            if p.len() < 3 {
                return None;
            }
            Some((p[0].to_string(), p[1].parse().ok()?, p[2].parse().ok()?))
        })
        .collect()
}

/// Модели .gguf в папке (вложенные папки тоже). Части большой модели
/// («-00002-of-00003») и проекторы изображений (mmproj) не показываются.
pub fn scan_models(dir: &str) -> Vec<Json> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out, depth + 1);
            } else if p.extension().map(|x| x.to_string_lossy().to_lowercase() == "gguf").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(Path::new(dir), &mut files, 0);
    let mut out = Vec::new();
    for f in files {
        let name = f.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
        let low = name.to_lowercase();
        if low.starts_with("mmproj") || low.contains("-mmproj") {
            continue;
        }
        // шардированные модели: показываем только первую часть, размер — суммарный
        let mut size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
        if let Some(i) = low.find("-of-") {
            let part = &low[..i];
            if !part.ends_with("-00001") {
                continue;
            }
            let prefix = &name[..i - 6];
            if let Some(parent) = f.parent() {
                if let Ok(rd) = std::fs::read_dir(parent) {
                    size = rd
                        .filter_map(|e| e.ok())
                        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
                        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                        .sum();
                }
            }
        }
        let has_mmproj = find_mmproj(&f).is_some();
        let mut stem = name.trim_end_matches(".gguf").to_string();
        if let Some(i) = stem.to_lowercase().find("-00001-of-") {
            stem.truncate(i);
        }
        let moe = {
            let l = stem.to_lowercase();
            l.contains("moe") || l.contains("-a3b") || l.contains("-a4b") || l.contains("-a6b") || l.contains("a3.") || l.contains("mixtral")
        };
        out.push(
            Json::obj()
                .with("path", f.to_string_lossy().to_string())
                .with("name", stem)
                .with("size_gb", (size as f64 / 1e9 * 10.0).round() / 10.0)
                .with("moe", moe)
                .with("vision", has_mmproj),
        );
    }
    out.sort_by(|a, b| a.get("name").str_or("").to_lowercase().cmp(&b.get("name").str_or("").to_lowercase()));
    out
}

/// mmproj-файл для модели: в той же папке, если модель там одна,
/// или с совпадающим началом имени («mmproj-Qwen2.5-VL-7B…» для «Qwen2.5-VL-7B-Q4_K_M»).
pub fn find_mmproj(model: &Path) -> Option<std::path::PathBuf> {
    let dir = model.parent()?;
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map(|e| e.eq_ignore_ascii_case("gguf")).unwrap_or(false)).collect();
    let is_mm = |p: &std::path::PathBuf| p.file_name().map(|n| n.to_string_lossy().to_lowercase().contains("mmproj")).unwrap_or(false);
    let mut mms: Vec<&std::path::PathBuf> = files.iter().filter(|p| is_mm(p)).collect();
    if mms.is_empty() {
        return None;
    }
    // f16/bf16 точнее, но больше; берём первый по имени — обычно f16
    mms.sort();
    let models: Vec<&std::path::PathBuf> = files
        .iter()
        .filter(|p| !is_mm(p))
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            !n.contains("-of-") || n.contains("-00001-of-")
        })
        .collect();
    if models.len() == 1 {
        return Some(mms[0].clone());
    }
    let stem = model.file_stem()?.to_string_lossy().to_lowercase();
    let key: String = stem.split(|c: char| c == '-' || c == '_' || c == '.').filter(|x| !x.is_empty()).take(2).collect::<Vec<_>>().join("-");
    if key.len() < 4 {
        return None;
    }
    mms.into_iter()
        .find(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default().replace(['_', '.'], "-");
            n.contains(&key.replace(['_', '.'], "-"))
        })
        .cloned()
}

fn port_free(p: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
}

fn log_tail(p: &Path, n: usize) -> String {
    let t = std::fs::read_to_string(p).unwrap_or_default();
    let lines: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Понятное объяснение частых ошибок llama-server.
fn explain(log: &str) -> &'static str {
    let l = log.to_lowercase();
    if l.contains("out of memory") || l.contains("cudamalloc failed") || l.contains("failed to allocate") {
        "Не хватило памяти видеокарты: уменьшите контекст, выберите «эксперты MoE в оперативную память» или запустите меньше моделей одновременно."
    } else if l.contains("unknown model architecture") || l.contains("unknown architecture") {
        "Эта версия llama.cpp не знает архитектуру модели — обновите llama.cpp (кнопка «Обновить llama.cpp»)."
    } else if l.contains("error while loading shared libraries") || l.contains("0xc0000135") || l.contains(".dll") {
        "Не найдены библиотеки CUDA: переустановите llama.cpp кнопкой в приложении или обновите драйвер NVIDIA."
    } else if l.contains("invalid argument") || l.contains("unknown argument") || l.contains("error: invalid") {
        "llama-server не понял параметры запуска — уберите лишнее из «Дополнительных параметров» или обновите llama.cpp."
    } else if l.contains("failed to load model") || l.contains("failed to open") {
        "Не удалось открыть файл модели: проверьте, что он скачан полностью."
    } else {
        "Подробности — в журнале сервера ниже."
    }
}

pub struct StartOpts {
    pub ctx: i64,
    pub gpu: String,
    pub kv_q8: bool,
    pub extra: String,
}

/// Запустить серверы для моделей и дождаться их готовности.
pub fn start(core: &Core, models: &[String], o: &StartOpts) -> Result<Vec<(String, u16)>, String> {
    let bin = find_bin(core).ok_or("llama-server не найден: нажмите «Скачать llama.cpp» или укажите папку, где он лежит")?;
    if models.is_empty() {
        return Err("Выберите хотя бы одну модель".into());
    }
    stop_all();
    let logs = home_dir(core).join("logs");
    let _ = std::fs::create_dir_all(&logs);
    let mut port = PORT_BASE;
    let mut started: Vec<(String, u16)> = Vec::new();
    for (k, m) in models.iter().enumerate() {
        while !port_free(port) && port < PORT_BASE + 200 {
            port += 1;
        }
        let mut stem = Path::new(m).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| format!("model{}", k + 1));
        if let Some(i) = stem.to_lowercase().find("-00001-of-") {
            stem.truncate(i);
        }
        let mut args: Vec<String> = vec![
            "-m".into(), m.clone(),
            "--host".into(), "127.0.0.1".into(),
            "--port".into(), port.to_string(),
            "-c".into(), o.ctx.to_string(),
            "-np".into(), "1".into(),
            "--jinja".into(),
            "-a".into(), stem.clone(),
        ];
        match o.gpu.as_str() {
            "cpu" => args.extend(["-ngl".into(), "0".into()]),
            "moe" => args.extend(["-ngl".into(), "999".into(), "--cpu-moe".into()]),
            _ => args.extend(["-ngl".into(), "999".into()]),
        }
        if o.kv_q8 {
            args.extend(["-ctk".into(), "q8_0".into(), "-ctv".into(), "q8_0".into()]);
        }
        // проектор изображений (mmproj) рядом с моделью — модель сможет читать фото и сканы
        if !o.extra.contains("--mmproj") && !o.extra.contains("-mm ") {
            if let Some(mm) = find_mmproj(Path::new(m)) {
                core.log(&format!("llama.cpp: {} — найден проектор изображений {}", stem, mm.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default()));
                args.extend(["--mmproj".into(), mm.to_string_lossy().to_string()]);
            }
        }
        args.extend(split_args(&o.extra));
        let safe: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect();
        let log = logs.join(format!("{}-{}.log", port, safe));
        let lf = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let lf2 = lf.try_clone().map_err(|e| e.to_string())?;
        let mut c = Command::new(&bin);
        c.args(&args).current_dir(bin.parent().unwrap_or(Path::new("."))).stdin(Stdio::null()).stdout(Stdio::from(lf)).stderr(Stdio::from(lf2));
        no_window(&mut c);
        core.log(&format!("llama.cpp: запускаю {} на порту {} (контекст {})", stem, port, o.ctx));
        (core.emit)("llama", Json::obj().with("phase", "start").with("model", stem.as_str()).with("message", format!("Запускаю {} ({} из {})", stem, k + 1, models.len())).to_string());
        let child = c.spawn().map_err(|e| format!("не удалось запустить llama-server: {}", e))?;
        servers().lock().unwrap_or_else(|e| e.into_inner()).push(Running { model: stem.clone(), path: m.clone(), port, ctx: o.ctx, child, log: log.clone() });
        // ждём загрузки модели (с медленного диска большая модель грузится минуты)
        let t0 = Instant::now();
        loop {
            if core.cancel.load(std::sync::atomic::Ordering::SeqCst) {
                stop_all();
                return Err("Запуск остановлен".into());
            }
            {
                let mut list = servers().lock().unwrap_or_else(|e| e.into_inner());
                if let Some(r) = list.iter_mut().find(|r| r.port == port) {
                    if let Ok(Some(st)) = r.child.try_wait() {
                        let tail = log_tail(&log, 12);
                        drop(list);
                        stop_all();
                        return Err(format!("{} не запустилась (код {}). {}\n\n{}", stem, st.code().unwrap_or(-1), explain(&tail), tail));
                    }
                }
            }
            if let Ok(r) = core.http.request("GET", &format!("http://127.0.0.1:{}/health", port), &[], None, 3) {
                if r.status == 200 {
                    break;
                }
            }
            if t0.elapsed() > Duration::from_secs(900) {
                stop_all();
                return Err(format!("{} не загрузилась за 15 минут. {}", stem, explain(&log_tail(&log, 12))));
            }
            let secs = t0.elapsed().as_secs();
            if secs % 5 == 0 {
                (core.emit)("llama", Json::obj().with("phase", "loading").with("model", stem.as_str()).with("message", format!("Загружаю {} в память… {} с", stem, secs)).to_string());
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        (core.emit)("llama", Json::obj().with("phase", "ready").with("model", stem.as_str()).with("message", format!("{} готова (порт {})", stem, port)).to_string());
        started.push((stem, port));
        port += 1;
    }
    Ok(started)
}

/// Разбор строки дополнительных параметров с учётом кавычек.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for c in s.chars() {
        match (q, c) {
            (Some(x), c) if c == x => q = None,
            (Some(_), c) => cur.push(c),
            (None, '"') | (None, '\'') => q = Some(c),
            (None, c) if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn status(core: &Core) -> Json {
    let bin = find_bin(core);
    let mut run = Json::arr();
    {
        let mut list = servers().lock().unwrap_or_else(|e| e.into_inner());
        list.retain_mut(|r| !matches!(r.child.try_wait(), Ok(Some(_))));
        for r in list.iter() {
            run.push(Json::obj().with("model", r.model.as_str()).with("path", r.path.as_str()).with("port", r.port as i64).with("ctx", r.ctx).with("log", r.log.to_string_lossy().to_string()));
        }
    }
    let gp: Vec<Json> = gpus().into_iter().map(|(n, t, u)| Json::obj().with("name", n).with("total_mb", t).with("used_mb", u)).collect();
    Json::obj()
        .with("bin", bin.map(|b| b.to_string_lossy().to_string()))
        .with("home", home_dir(core).to_string_lossy().to_string())
        .with("running", run)
        .with("gpus", Json::Arr(gp))
}

// ------------------------------------------------------------------ установка

/// Скачать свежую сборку llama.cpp для Windows с GitHub (CUDA — для видеокарт NVIDIA).
/// `variant`: cuda (новейшая CUDA), cuda12, vulkan, cpu.
pub fn install(core: &Core, variant: &str) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("Автоматическая установка llama.cpp есть только для Windows. Установите llama-server вручную и укажите папку.".into());
    }
    let r = core.http.request("GET", "https://api.github.com/repos/ggml-org/llama.cpp/releases/latest", &[("Accept".into(), "application/vnd.github+json".into()), ("User-Agent".into(), "proverka-zadaniy".into())], None, 30)?;
    if r.status >= 400 {
        return Err(format!("GitHub ответил {}: {}", r.status, r.body.chars().take(200).collect::<String>()));
    }
    let j = json::parse(&r.body)?;
    let tag = j.get("tag_name").str_or("");
    let assets: Vec<(String, String, i64)> = j
        .get("assets")
        .as_arr()
        .iter()
        .map(|a| (a.get("name").str_or(""), a.get("browser_download_url").str_or(""), a.get("size").i64_or(0)))
        .collect();
    let find = |pred: &dyn Fn(&str) -> bool| assets.iter().find(|a| pred(&a.0.to_lowercase())).cloned();
    let cuda_ver = |n: &str| -> f64 { n.split("cuda-").nth(1).and_then(|r| r.split('-').next()).and_then(|v| v.parse().ok()).unwrap_or(0.0) };
    let mut want: Vec<(String, String, i64)> = Vec::new();
    // основа: llama-server.exe и общие библиотеки
    if let Some(cpu) = find(&|n| n.contains("bin-win-cpu-x64") && n.ends_with(".zip")) {
        want.push(cpu);
    }
    match variant {
        "vulkan" => want.extend(find(&|n| n.contains("bin-win-vulkan-x64") && n.ends_with(".zip"))),
        "cpu" => {}
        _ => {
            let mut cudas: Vec<&(String, String, i64)> = assets.iter().filter(|a| {
                let n = a.0.to_lowercase();
                n.contains("bin-win-cuda-") && n.contains("x64") && !n.contains("cudart") && n.ends_with(".zip")
            }).collect();
            cudas.sort_by(|a, b| cuda_ver(&b.0.to_lowercase()).partial_cmp(&cuda_ver(&a.0.to_lowercase())).unwrap_or(std::cmp::Ordering::Equal));
            let pick = if variant == "cuda12" { cudas.iter().find(|a| cuda_ver(&a.0.to_lowercase()) < 13.0).copied() } else { cudas.first().copied() };
            let Some(c) = pick else { return Err("В свежем выпуске llama.cpp не нашлось сборки CUDA для Windows".into()) };
            let v = cuda_ver(&c.0.to_lowercase());
            want.push(c.clone());
            if let Some(rt) = assets.iter().find(|a| {
                let n = a.0.to_lowercase();
                n.contains("cudart") && n.contains("win") && (cuda_ver(&n) - v).abs() < 0.001
            }) {
                want.push(rt.clone());
            }
        }
    }
    if want.is_empty() {
        return Err("Не нашлось подходящей сборки llama.cpp для Windows".into());
    }
    let dest = home_dir(core).join(format!("{}-{}", if tag.is_empty() { "latest" } else { &tag }, variant));
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let dl = home_dir(core).join("download");
    let _ = std::fs::create_dir_all(&dl);
    let total: i64 = want.iter().map(|a| a.2).sum();
    let mut done_before: i64 = 0;
    for (name, url, size) in &want {
        let file = dl.join(name);
        let emit = core.emit.clone();
        let label = name.clone();
        let base = done_before;
        core.http.download(url, &file, &move |got: u64, _len: u64| {
            emit("llama", Json::obj().with("phase", "download").with("message", format!("Скачиваю {}…", label)).with("done", base + got as i64).with("total", total).to_string());
        })?;
        done_before += size;
        (core.emit)("llama", Json::obj().with("phase", "unpack").with("message", format!("Распаковываю {}…", name)).to_string());
        unpack(&file, &dest)?;
        let _ = std::fs::remove_file(&file);
    }
    let bin = find_in(&dest, 2).ok_or("После распаковки llama-server.exe не найден")?;
    // запоминаем папку
    {
        let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
        let mut l = st.get("llama").clone();
        if l.is_null() {
            l = Json::obj();
        }
        l.set("bin_dir", bin.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
        st.set("llama", l);
    }
    core.save_settings();
    core.log(&format!("llama.cpp {} ({}) установлен: {}", tag, variant, bin.display()));
    Ok(bin.to_string_lossy().to_string())
}

/// Распаковка: встроенный в Windows tar (быстро), иначе — свой распаковщик.
fn unpack(zip: &Path, dest: &Path) -> Result<(), String> {
    let mut c = Command::new("tar");
    c.arg("-xf").arg(zip).arg("-C").arg(dest);
    no_window(&mut c);
    if let Ok(o) = c.output() {
        if o.status.success() {
            return Ok(());
        }
    }
    let data = std::fs::read(zip).map_err(|e| e.to_string())?;
    let z = super::zip::ZipArchive::open(&data)?;
    for name in z.names() {
        if name.ends_with('/') || name.contains("..") {
            continue;
        }
        if let Some(b) = z.read(name) {
            let out = dest.join(name);
            if let Some(p) = out.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            std::fs::write(&out, b).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Версия и видеокарты, которые видит llama-server.
pub fn devices(core: &Core) -> String {
    let Some(bin) = find_bin(core) else { return String::new() };
    let mut c = Command::new(&bin);
    c.arg("--list-devices").current_dir(bin.parent().unwrap_or(Path::new(".")));
    no_window(&mut c);
    let Ok(mut ch) = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() else { return String::new() };
    let t0 = Instant::now();
    loop {
        if let Ok(Some(_)) = ch.try_wait() {
            break;
        }
        if t0.elapsed() > Duration::from_secs(20) {
            let _ = ch.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut s = String::new();
    if let Some(mut o) = ch.stdout.take() {
        let _ = o.read_to_string(&mut s);
    }
    if let Some(mut e) = ch.stderr.take() {
        let _ = e.read_to_string(&mut s);
    }
    s.lines().filter(|l| !l.trim().is_empty()).take(15).collect::<Vec<_>>().join("\n")
}

/// Подключить запущенные серверы к проверке: первая модель — основная,
/// остальные — дополнительные проверяющие.
pub fn register(core: &Core, started: &[(String, u16)], ctx: i64) {
    let mut st = core.settings.lock().unwrap_or_else(|e| e.into_inner());
    let mut provs: Vec<Json> = st.get("providers").as_arr().iter().filter(|p| !p.get("id").str_or("").starts_with(AUTO_PREFIX)).cloned().collect();
    let mut ids = Vec::new();
    for (k, (model, port)) in started.iter().enumerate() {
        let id = format!("{}-{}", AUTO_PREFIX, k + 1);
        provs.push(
            Json::obj()
                .with("id", id.as_str())
                .with("kind", "llamacpp")
                .with("name", format!("llama.cpp {}", k + 1))
                .with("base_url", format!("http://127.0.0.1:{}/v1", port))
                .with("model", model.as_str())
                .with("temperature", 0.1)
                .with("max_tokens", 3000)
                .with("num_ctx", ctx)
                .with("structured", true)
                .with("timeout_s", 900)
                .with("local", true)
                .with("no_think", true),
        );
        ids.push((id, model.clone()));
    }
    st.set("providers", Json::Arr(provs));
    if let Some((id, _)) = ids.first() {
        st.set("active_provider", id.as_str());
    }
    let mut dc = st.get("double_check").clone();
    if dc.is_null() {
        dc = Json::obj();
    }
    // проверяют ровно запущенные модели: первая — основная, остальные — дополнительные
    let mut checkers: Vec<Json> = Vec::new();
    for (id, model) in ids.iter().skip(1) {
        checkers.push(Json::obj().with("provider", id.as_str()).with("model", model.as_str()).with("num_ctx", ctx));
    }
    dc.set("enabled", !checkers.is_empty());
    dc.set("provider", checkers.first().map(|c| c.get("provider").str_or("")).unwrap_or_default());
    dc.set("model", checkers.first().map(|c| c.get("model").str_or("")).unwrap_or_default());
    dc.set("checkers", Json::Arr(checkers));
    st.set("double_check", dc);
    drop(st);
    core.save_settings();
}

#[cfg(test)]
mod tests {
    #[test]
    fn args() {
        assert_eq!(super::split_args(r#"--n-cpu-moe 20 --override-tensor "a b""#), vec!["--n-cpu-moe", "20", "--override-tensor", "a b"]);
    }
}
