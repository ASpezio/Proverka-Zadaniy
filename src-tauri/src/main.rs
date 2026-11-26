// Оболочка Tauri 2 для «Проверки заданий».
// Вся логика — в модуле engine (чистый Rust). Здесь только:
//  • окно приложения и одна команда `api` для интерфейса;
//  • HTTP-клиент с HTTPS (ureq) для облачных нейросетей;
//  • режим `--mcp` (MCP-сервер по stdio для внешних клиентов).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;

use engine::core::Core;
use engine::http::{HttpClient, HttpResponse, RawResponse, RenderedPage};
use std::sync::Arc;
use std::time::Duration;
use tauri::Emitter;

/// HTTP(S)-клиент на ureq: локальные модели и облачные API.
struct UreqClient;

/// Приложение — для встроенного браузера, которым открываются ссылки из работ.
static APP: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

/// Скрипт в окне чтения ссылки: через несколько секунд после загрузки отдаёт
/// видимый текст страницы переходом на служебный адрес (переход отменяется,
/// а текст забирает приложение). Окнам со страницами сайтов доступа к
/// командам приложения нет.
const CAPTURE_JS: &str = r#"(function () {
  if (window.top !== window) return;
  try { window.alert = function () {}; window.confirm = function () { return false; }; window.prompt = function () { return null; }; window.open = function () { return null; }; } catch (e) {}
  function mute() { try { document.querySelectorAll("video,audio").forEach(function (m) { m.muted = true; m.pause && m.pause(); }); } catch (e) {} }
  setInterval(mute, 700);
  function send() {
    try {
      var b = document.body;
      var t = b ? (b.innerText || "") : "";
      var d = JSON.stringify({ u: location.href, t: document.title || "", x: t.slice(0, 60000) });
      location.href = "https://proverka-capture.invalid/?d=" + encodeURIComponent(d);
    } catch (e) {}
  }
  function start() { setTimeout(send, 2500); setTimeout(send, 7000); }
  if (document.readyState === "complete") start(); else window.addEventListener("load", start);
})();"#;

impl HttpClient for UreqClient {
    /// Страница с защитой от роботов или на JavaScript: открываем в отдельном
    /// невидимом для преподавателя окне WebView2 (за пределами экрана, без значка
    /// на панели задач) и забираем текст, как его видит человек.
    fn render_page(&self, url: &str, timeout_s: u64) -> Result<RenderedPage, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Mutex;
        static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
        static N: AtomicU64 = AtomicU64::new(1);
        let app = APP.get().ok_or("окно приложения ещё не готово")?;
        engine::links::allowed(url)?;
        let parsed: tauri::Url = url.parse().map_err(|_| "неверная ссылка".to_string())?;
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        let got: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        let label = format!("linkreader{}", N.fetch_add(1, Ordering::SeqCst));
        let win = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::External(parsed))
            .title("Проверка заданий — чтение ссылки")
            .inner_size(1280.0, 900.0)
            .position(-32000.0, -32000.0)
            .skip_taskbar(true)
            .focused(false)
            .decorations(false)
            .visible(true)
            .initialization_script(CAPTURE_JS)
            .on_navigation(move |u: &tauri::Url| {
                if u.host_str() == Some("proverka-capture.invalid") {
                    let d = u.query_pairs().find(|(k, _)| k == "d").map(|(_, v)| v.into_owned()).unwrap_or_default();
                    if let Ok(j) = engine::json::parse(&d) {
                        sink.lock().unwrap_or_else(|e| e.into_inner()).push((j.get("u").str_or(""), j.get("t").str_or(""), j.get("x").str_or("")));
                    }
                    return false;
                }
                // переход в локальную сеть со страницы студента не пускаем
                engine::links::allowed(u.as_str()).is_ok()
            })
            .build()
            .map_err(|e| format!("не удалось открыть окно браузера: {}", e))?;
        let t0 = std::time::Instant::now();
        let limit = Duration::from_secs(timeout_s.clamp(10, 60));
        let mut good_since: Option<std::time::Instant> = None;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let snaps = got.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let good = snaps.iter().any(|(_, t, x)| !engine::links::looks_blocked(t, x));
            if good && good_since.is_none() {
                good_since = Some(std::time::Instant::now());
            }
            // после первого нормального снимка ждём второй (страница дорисовывается)
            if good_since.map(|g| g.elapsed() > Duration::from_secs(5)).unwrap_or(false) || t0.elapsed() > limit {
                break;
            }
        }
        let _ = win.destroy();
        let snaps = got.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let best = snaps
            .iter()
            .filter(|(_, t, x)| !engine::links::looks_blocked(t, x))
            .max_by_key(|(_, _, x)| x.chars().count())
            .or(snaps.last())
            .cloned()
            .ok_or("страница не загрузилась за отведённое время")?;
        Ok(RenderedPage { url: best.0, title: best.1, text: best.2 })
    }

    /// Ссылки из работ студентов: без автоматических переадресаций (каждую
    /// проверяет ядро — чтобы ссылка не увела в локальную сеть), с пределом размера.
    fn get_raw(&self, url: &str, headers: &[(String, String)], max_bytes: usize, timeout_s: u64) -> Result<RawResponse, String> {
        use std::io::Read;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(timeout_s.max(5)))
            .redirects(0)
            .build();
        let mut req = agent
            .get(url)
            .set("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .set("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
            .set("Accept-Language", "ru-RU,ru;q=0.9,en;q=0.6")
            .set("Upgrade-Insecure-Requests", "1")
            .set("Sec-Fetch-Dest", "document")
            .set("Sec-Fetch-Mode", "navigate")
            .set("Sec-Fetch-Site", "none");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let r = match req.call() {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(friendly(&e.to_string())),
        };
        let status = r.status();
        // все значения (Set-Cookie обычно приходит несколько раз)
        let mut names = r.headers_names();
        names.sort();
        names.dedup();
        let hdrs: Vec<(String, String)> = names.iter().flat_map(|n| r.all(n).into_iter().map(move |v| (n.to_lowercase(), v.to_string()))).collect();
        let mut body = Vec::new();
        r.into_reader().take(max_bytes as u64 + 1).read_to_end(&mut body).map_err(|e| format!("обрыв загрузки: {}", e))?;
        Ok(RawResponse { status, headers: hdrs, body })
    }

    fn request(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<&str>, timeout_s: u64) -> Result<HttpResponse, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(8))
            .timeout_read(Duration::from_secs(timeout_s.max(5)))
            .timeout_write(Duration::from_secs(60))
            .build();
        // Windows сначала пробует IPv6 (::1) и ждёт ~2 с на каждом отказе, а LM Studio,
        // Ollama и llama-server слушают 127.0.0.1 — поэтому localhost подменяем сразу.
        let url = url.replacen("://localhost:", "://127.0.0.1:", 1).replacen("://localhost/", "://127.0.0.1/", 1);
        let url = url.as_str();
        let local = url.contains("://127.0.0.1") || url.contains("://[::1]");
        let agent = if local {
            ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout_read(Duration::from_secs(timeout_s.max(5)))
                .timeout_write(Duration::from_secs(60))
                .build()
        } else {
            agent
        };
        let mut req = agent.request(method, url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let res = match body {
            Some(b) => req.send_string(b),
            None => req.call(),
        };
        match res {
            Ok(r) => {
                let status = r.status();
                let text = r.into_string().map_err(|e| e.to_string())?;
                Ok(HttpResponse { status, body: text })
            }
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                Ok(HttpResponse { status: code, body: text })
            }
            Err(e) => Err(friendly(&e.to_string())),
        }
    }

    fn download(&self, url: &str, dest: &std::path::Path, progress: &dyn Fn(u64, u64)) -> Result<u64, String> {
        use std::io::{Read, Write};
        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(120)).redirects(8).build();
        let r = agent.get(url).set("User-Agent", "proverka-zadaniy").call().map_err(|e| format!("не удалось скачать: {}", e))?;
        let len: u64 = r.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let tmp = dest.with_extension("part");
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut rd = r.into_reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut got: u64 = 0;
        let mut last = std::time::Instant::now();
        loop {
            let n = rd.read(&mut buf).map_err(|e| format!("обрыв скачивания: {}", e))?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            got += n as u64;
            if last.elapsed() > Duration::from_millis(400) {
                progress(got, len);
                last = std::time::Instant::now();
            }
        }
        drop(f);
        if len > 0 && got < len {
            return Err("скачивание оборвалось — попробуйте ещё раз".into());
        }
        std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
        progress(got, len);
        Ok(got)
    }
}

fn friendly(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("connection refused") || l.contains("отказ") || l.contains("10061") {
        format!("сервер модели не запущен или недоступен ({})", e)
    } else if l.contains("timed out") || l.contains("timeout") {
        format!("истекло время ожидания ответа модели — увеличьте таймаут или выберите модель поменьше ({})", e)
    } else {
        e.to_string()
    }
}

#[tauri::command]
async fn api(state: tauri::State<'_, Arc<Core>>, method: String, payload: String) -> Result<String, String> {
    let core = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine::api::dispatch(&core, &method, &payload))
        .await
        .map_err(|e| e.to_string())?
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = engine::core::default_data_dir();

    // MCP-сервер для внешних клиентов (LM Studio, Claude Desktop, Cursor…)
    if args.iter().any(|a| a == "--mcp") {
        let core = Core::new(data_dir, Arc::new(UreqClient), Arc::new(|_: &str, _: String| {}));
        engine::mcp::run_stdio(core);
        return;
    }

    let start_hidden = args.iter().any(|a| a == "--tray");

    tauri::Builder::default()
        // второй запуск не открывает второе окно (иначе две копии спорят за сессию),
        // а просто выводит уже открытое на передний план (в том числе из трея)
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            use tauri::Manager;
            let handle = app.handle().clone();
            let _ = APP.set(handle.clone());
            let emitter: engine::core::Emitter = Arc::new(move |event: &str, payload: String| {
                // значок в трее: сколько проверено, сколько осталось, кнопка «Остановить»
                if event == "run_state" {
                    if let Ok(j) = engine::json::parse(&payload) {
                        tray_progress(&j);
                        refresh_tray(&handle);
                    }
                }
                if event == "settings_saved" {
                    refresh_tray(&handle);
                    return;
                }
                // уведомление Windows — если включено и (по настройке) окно не перед глазами
                if event == "notify" {
                    // предупреждения и вопросы — всегда в окне приложения; Windows — только если окно не перед глазами
                    let _ = handle.emit("notice", payload.clone());
                    show_notification(&handle, &payload);
                    return;
                }
                let _ = handle.emit(event, payload);
            });
            let core = Core::new(data_dir.clone(), Arc::new(UreqClient), emitter);
            app.manage(core);
            register_toast_id(&data_dir, &app.config().identifier);
            build_tray(app)?;
            // окно создаётся скрытым: при автозапуске «свёрнутым в трей» оно так и остаётся
            if let Some(w) = app.get_webview_window("main") {
                if !start_hidden {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            use tauri::Manager;
            if window.label() != "main" {
                return;
            }
            let pref = |key: &str| window.app_handle().state::<Arc<Core>>().settings().get("tray").get(key).bool_or(false);
            match event {
                // «Закрыть» уводит в трей (как в Spotify), если это включено в настройках
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    if !QUITTING.load(std::sync::atomic::Ordering::SeqCst) && pref("close_to_tray") {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
                // свернуть — тоже в трей
                tauri::WindowEvent::Resized(_) => {
                    if pref("minimize_to_tray") && window.is_minimized().unwrap_or(false) {
                        let _ = window.hide();
                    }
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![api])
        .build(tauri::generate_context!())
        .expect("не удалось запустить приложение")
        .run(|_app, ev| {
            // серверы llama.cpp, запущенные приложением, не должны остаться висеть
            if let tauri::RunEvent::Exit = ev {
                engine::llamacpp::stop_all();
            }
        });
}

/// Уведомление Windows по событию ядра `notify` { kind, title, body }.
fn show_notification<R: tauri::Runtime>(app: &tauri::AppHandle<R>, payload: &str) {
    use tauri::Manager;
    use tauri_plugin_notification::NotificationExt;
    let Ok(j) = engine::json::parse(payload) else { return };
    let Some(core) = app.try_state::<Arc<Core>>() else { return };
    let st = core.settings();
    if !engine::notify::wanted(&st, &j.get("kind").str_or("")) {
        return;
    }
    // окно перед глазами — сообщение уже показано в самом приложении, отдельное уведомление Windows не нужно
    if j.get("kind").str_or("") != "test" {
        let in_front = app
            .get_webview_window("main")
            .map(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false) && w.is_focused().unwrap_or(false))
            .unwrap_or(false);
        if in_front {
            return;
        }
    }
    let _ = app.notification().builder().title(j.get("title").str_or("Проверка заданий")).body(j.get("body").str_or("")).show();
}

/// Windows показывает всплывающие уведомления только от «известного» приложения:
/// его код (AppUserModelID, у нас — identifier из tauri.conf.json) должен быть
/// записан в реестре, иначе уведомления молча пропадают. Поэтому при запуске приложение
/// само регистрирует себя в HKCU (права администратора не нужны): имя и значок
/// для уведомлений.
#[cfg(windows)]
fn register_toast_id(data_dir: &std::path::Path, id: &str) {
    use std::os::windows::process::CommandExt;
    const NO_WINDOW: u32 = 0x0800_0000;
    let id = id.to_string();
    let icon = data_dir.join("app-icon.png");
    std::thread::spawn(move || {
        let _ = std::fs::create_dir_all(icon.parent().unwrap_or(std::path::Path::new(".")));
        if !icon.exists() {
            let _ = std::fs::write(&icon, include_bytes!("../icons/128x128.png"));
        }
        let key = format!("HKCU\\Software\\Classes\\AppUserModelId\\{id}");
        let set = |name: &str, value: &str| {
            let _ = std::process::Command::new("reg")
                .args(["add", &key, "/v", name, "/t", "REG_SZ", "/d", value, "/f"])
                .creation_flags(NO_WINDOW)
                .status();
        };
        set("DisplayName", "Проверка заданий");
        set("IconUri", &icon.to_string_lossy());
        set("IconBackgroundColor", "FF1B2140");
    });
}

#[cfg(not(windows))]
fn register_toast_id(_data_dir: &std::path::Path, _id: &str) {}

/// Пользователь выбрал «Выход» в меню трея — окно закрывается по-настоящему.
static QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn show_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Что показывает значок в трее о текущей проверке.
struct TrayInfo {
    running: bool,
    done: i64,
    total: i64,
    started: Option<std::time::Instant>,
    last: String,
}

static TRAY: std::sync::Mutex<TrayInfo> = std::sync::Mutex::new(TrayInfo { running: false, done: 0, total: 0, started: None, last: String::new() });

/// Запомнить ход проверки из события `run_state`.
fn tray_progress(j: &engine::json::Json) {
    let mut t = TRAY.lock().unwrap_or_else(|e| e.into_inner());
    let running = j.get("running").bool_or(false);
    if running {
        if !t.running || t.started.is_none() {
            t.started = Some(std::time::Instant::now());
        }
        t.done = j.get("done").i64_or(t.done);
        t.total = j.get("total").i64_or(t.total);
    } else if t.running {
        let total = j.get("total").i64_or(t.total);
        t.last = if j.get("cancelled").bool_or(false) {
            format!("проверка остановлена: {} из {}", t.done, total)
        } else {
            format!("проверка завершена: {} из {}", total, total)
        };
        t.started = None;
    }
    t.running = running;
}

/// Строка о ходе проверки: количеством, процентами или сколько осталось.
fn tray_status(mode: &str, full: bool) -> String {
    let t = TRAY.lock().unwrap_or_else(|e| e.into_inner());
    if !t.running {
        return if t.last.is_empty() { "проверка не идёт".to_string() } else { t.last.clone() };
    }
    let (d, n) = (t.done, t.total.max(1));
    let pct = d * 100 / n;
    let left = (n - d).max(0);
    // примерное время до конца — по средней скорости этой проверки
    let eta = match t.started {
        Some(st) if d > 0 && left > 0 => {
            let sec = st.elapsed().as_secs_f64() / d as f64 * left as f64;
            if sec < 60.0 { " · меньше минуты".to_string() } else { format!(" · ≈ {} мин", (sec / 60.0).round() as i64) }
        }
        _ => String::new(),
    };
    if full {
        return format!("Проверено {} из {} ({} %) · осталось {}{}", d, n, pct, left, eta);
    }
    match mode {
        "percent" => format!("проверено {} %{}", pct, eta),
        "left" => format!("осталось {} из {}{}", left, n, eta),
        _ => format!("проверено {} из {}{}", d, n, eta),
    }
}

/// Обновить подсказку, меню трея и полоску прогресса на кнопке в панели задач.
fn refresh_tray(app: &tauri::AppHandle) {
    use tauri::Manager;
    let (sound, mode) = match app.try_state::<Arc<Core>>() {
        Some(c) => {
            let st = c.settings();
            (st.get("sound").bool_or(true), st.get("tray").get("progress").str_or("count"))
        }
        None => (true, "count".to_string()),
    };
    let (running, pct) = {
        let t = TRAY.lock().unwrap_or_else(|e| e.into_inner());
        (t.running, if t.total > 0 { (t.done * 100 / t.total).clamp(0, 100) as u64 } else { 0 })
    };
    if let Some(tray) = app.tray_by_id("main") {
        let tip = format!("Проверка заданий — {}", tray_status(&mode, false));
        let _ = tray.set_tooltip(Some(tip.as_str()));
        if let Ok(menu) = tray_menu(app, sound, &mode, running) {
            let _ = tray.set_menu(Some(menu));
        }
    }
    if let Some(w) = app.get_webview_window("main") {
        use tauri::window::{ProgressBarState, ProgressBarStatus};
        let state = if running {
            ProgressBarState { status: Some(ProgressBarStatus::Normal), progress: Some(pct) }
        } else {
            ProgressBarState { status: Some(ProgressBarStatus::None), progress: None }
        };
        let _ = w.set_progress_bar(state);
    }
}

/// Меню значка: ход проверки, «Остановить», звук, как показывать прогресс, выход.
fn tray_menu<R: tauri::Runtime, M: tauri::Manager<R>>(app: &M, sound: bool, mode: &str, running: bool) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
    let show = MenuItem::with_id(app, "show", "Открыть «Проверку заданий»", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", tray_status(mode, true), false, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Остановить проверку", running, None::<&str>)?;
    let snd = CheckMenuItem::with_id(app, "sound", "Звуки", true, sound, None::<&str>)?;
    let pm_count = CheckMenuItem::with_id(app, "pm_count", "Количеством: «проверено 5 из 12»", true, mode != "percent" && mode != "left", None::<&str>)?;
    let pm_percent = CheckMenuItem::with_id(app, "pm_percent", "Процентами: «проверено 42 %»", true, mode == "percent", None::<&str>)?;
    let pm_left = CheckMenuItem::with_id(app, "pm_left", "Сколько осталось: «осталось 7 из 12»", true, mode == "left", None::<&str>)?;
    let progress = Submenu::with_items(app, "Ход проверки в подсказке", true, &[&pm_count, &pm_percent, &pm_left])?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    Menu::with_items(app, &[&show, &sep1, &status, &stop, &sep2, &snd, &progress, &sep3, &quit])
}

/// Команда меню трея меняет настройку так же, как интерфейс, и сообщает об этом окну.
fn tray_setting(app: &tauri::AppHandle, patch: engine::json::Json) {
    use tauri::Manager;
    if let Some(core) = app.try_state::<Arc<Core>>() {
        let core = core.inner().clone();
        let payload = engine::json::Json::obj().with("settings", patch).to_string();
        let _ = engine::api::dispatch(&core, "set_settings", &payload);
        let _ = app.emit("settings_changed", "{}".to_string());
    }
    refresh_tray(app);
}

/// Значок в трее: щелчок — открыть окно; меню — ход проверки, звук, выход.
fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::Manager;
    let (sound, mode) = match app.try_state::<Arc<Core>>() {
        Some(c) => {
            let st = c.settings();
            (st.get("sound").bool_or(true), st.get("tray").get("progress").str_or("count"))
        }
        None => (true, "count".to_string()),
    };
    let menu = tray_menu(app, sound, &mode, false)?;
    let mut b = TrayIconBuilder::with_id("main").tooltip("Проверка заданий").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.on_menu_event(|app, ev| match ev.id().as_ref() {
        "show" => show_main(app),
        "stop" => {
            if let Some(core) = app.try_state::<Arc<Core>>() {
                let core = core.inner().clone();
                let _ = engine::api::dispatch(&core, "cancel_check", "{}");
            }
        }
        "sound" => {
            let on = app.try_state::<Arc<Core>>().map(|c| c.settings().get("sound").bool_or(true)).unwrap_or(true);
            tray_setting(app, engine::json::Json::obj().with("sound", !on));
        }
        "pm_count" | "pm_percent" | "pm_left" => {
            let m = ev.id().as_ref().trim_start_matches("pm_").to_string();
            tray_setting(app, engine::json::Json::obj().with("tray", engine::json::Json::obj().with("progress", m)));
        }
        "quit" => {
            QUITTING.store(true, std::sync::atomic::Ordering::SeqCst);
            engine::llamacpp::stop_all();
            app.exit(0);
        }
        _ => {}
    })
    .on_tray_icon_event(|tray, ev| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
            show_main(tray.app_handle());
        }
    })
    .build(app)?;
    Ok(())
}
