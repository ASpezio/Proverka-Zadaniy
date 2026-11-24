//! Тестовый стенд ядра «Проверка заданий» (только std).
//!   devserver check <папка_заданий> <папка_работ>   — разбор и автоматические проверки
//!   devserver serve [порт] [папка_ui]                — интерфейс в браузере (http://127.0.0.1:порт)
//!   devserver mcp                                    — MCP-сервер по stdio

#[path = "../../../src-tauri/src/engine/mod.rs"]
#[allow(dead_code)]
mod engine;

use engine::core::Core;
use engine::json::Json;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};

type Queue = Arc<(Mutex<Vec<Json>>, Condvar)>;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("serve");
    let queue: Queue = Arc::new((Mutex::new(Vec::new()), Condvar::new()));
    let q2 = queue.clone();
    let quiet = mode == "mcp";
    let emitter: engine::core::Emitter = Arc::new(move |ev: &str, payload: String| {
        if quiet {
            return;
        }
        let (m, cv) = &*q2;
        let mut v = m.lock().unwrap_or_else(|e| e.into_inner());
        v.push(Json::obj().with("event", ev).with("payload", payload));
        if v.len() > 5000 {
            v.drain(0..1000);
        }
        cv.notify_all();
    });
    let data_dir = std::env::var("PROVERKA_DATA").map(std::path::PathBuf::from).unwrap_or_else(|_| engine::core::default_data_dir());
    let core = Core::new(data_dir, Arc::new(engine::http::StdHttp), emitter);
    match mode {
        "check" => check(&core, &args[2], &args[3]),
        "mcp" => engine::mcp::run_stdio(core),
        _ => {
            let port = args.get(2).cloned().unwrap_or_else(|| "8765".into());
            let ui = args.get(3).cloned().unwrap_or_else(|| "ui".into());
            serve(core, &port, &ui, queue);
        }
    }
}

fn check(core: &Arc<Core>, tasks: &str, works: &str) {
    core.add_tasks(&[tasks.to_string()], None, "");
    core.add_works(&[works.to_string()], None, "");
    core.compute_plagiarism();
    let st = core.state_json();
    println!("{}", st.get("sets").pretty());
    let ws = core.ws.lock().unwrap_or_else(|e| e.into_inner());
    for w in &ws.works {
        let sc = core.scan(&ws, w);
        println!("\n=== {}/{} | вариант: имя={:?} текст={:?} → {:?}, по содержанию {:?} ({:.2}), слов {}, заданий {}, скрытых {}",
            w.group, w.name, sc.variant_in_name, sc.variant_in_text, sc.variant_used, sc.best_match_variant, sc.best_match_score, sc.word_count, sc.task_headings, w.hidden.len());
        for e in &sc.evidence {
            println!("   [{}] {} :: {}", e.weight, e.what, e.snippet.chars().take(140).collect::<String>());
        }
        for (n, s) in core.plag_for(&ws, &w.id) {
            println!("   [plag] {} {:.0}%", n, s * 100.0);
        }
    }
}

fn serve(core: Arc<Core>, port: &str, ui: &str, queue: Queue) {
    let l = TcpListener::bind(format!("127.0.0.1:{}", port)).expect("bind");
    eprintln!("devserver: http://127.0.0.1:{}  (ui: {})", port, ui);
    for s in l.incoming().flatten() {
        let core = core.clone();
        let ui = ui.to_string();
        let queue = queue.clone();
        std::thread::spawn(move || {
            let _ = handle(s, core, &ui, queue);
        });
    }
}

fn handle(mut s: TcpStream, core: Arc<Core>, ui: &str, queue: Queue) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let (method, path) = (parts[0].to_string(), parts[1].to_string());
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        r.read_line(&mut h)?;
        if h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).to_string();
    let (status, ctype, out): (u16, &str, Vec<u8>) = if method == "POST" && path.starts_with("/api/") {
        let m = &path[5..];
        let c = core.clone();
        let m2 = m.to_string();
        match std::thread::spawn(move || engine::api::dispatch(&c, &m2, &body)).join() {
            Ok(Ok(j)) => (200, "application/json", j.into_bytes()),
            Ok(Err(e)) => (400, "application/json", Json::obj().with("error", e).to_string().into_bytes()),
            Err(_) => (500, "application/json", b"{\"error\":\"panic\"}".to_vec()),
        }
    } else if path.starts_with("/events") {
        let (m, cv) = &*queue;
        let mut v = m.lock().unwrap_or_else(|e| e.into_inner());
        if v.is_empty() {
            let (g, _) = cv.wait_timeout(v, std::time::Duration::from_secs(20)).unwrap();
            v = g;
        }
        let items: Vec<Json> = v.drain(..).collect();
        (200, "application/json", Json::Arr(items).to_string().into_bytes())
    } else {
        let rel = path.split('?').next().unwrap_or("/").trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        let rel = percent_decode(rel);
        let p = std::path::Path::new(ui).join(&rel);
        match std::fs::read(&p) {
            Ok(b) => {
                let ct = match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
                    "html" => "text/html; charset=utf-8",
                    "js" | "mjs" => "text/javascript; charset=utf-8",
                    "css" => "text/css; charset=utf-8",
                    "svg" => "image/svg+xml",
                    "png" => "image/png",
                    "woff2" => "font/woff2",
                    _ => "application/octet-stream",
                };
                (200, ct, b)
            }
            Err(_) => (404, "text/plain", b"not found".to_vec()),
        }
    };
    write!(s, "HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", status, ctype, out.len())?;
    s.write_all(&out)?;
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
