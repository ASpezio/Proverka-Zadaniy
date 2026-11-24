//! HTTP-клиент. В приложении используется реализация на `ureq` (HTTPS для облака),
//! здесь — интерфейс и простая реализация на std (только http://) для тестов
//! и для локальных моделей, если ureq недоступен.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Ответ без обработки: код, заголовки (имена в нижнем регистре), байты тела.
pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Страница, открытая во встроенном браузере (с JavaScript).
pub struct RenderedPage {
    pub url: String,
    pub title: String,
    pub text: String,
}

pub trait HttpClient: Send + Sync {
    /// Открыть страницу в скрытом окне браузера (WebView2) и вернуть видимый текст.
    /// Нужен для сайтов с защитой от роботов и страниц, которые рисует JavaScript.
    fn render_page(&self, url: &str, timeout_s: u64) -> Result<RenderedPage, String> {
        let _ = (url, timeout_s);
        Err("встроенный браузер здесь недоступен".into())
    }
    /// GET без переходов по переадресации (их проверяет вызывающий), тело — не больше `max_bytes`.
    fn get_raw(&self, url: &str, headers: &[(String, String)], max_bytes: usize, timeout_s: u64) -> Result<RawResponse, String> {
        let _ = (url, headers, max_bytes, timeout_s);
        Err("открытие ссылок здесь недоступно".into())
    }
    fn request(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<&str>, timeout_s: u64) -> Result<HttpResponse, String>;
    /// Скачать файл (с переходами по redirect) потоком на диск; progress(получено, всего).
    fn download(&self, _url: &str, _dest: &std::path::Path, _progress: &dyn Fn(u64, u64)) -> Result<u64, String> {
        Err("скачивание файлов здесь недоступно".into())
    }
}

/// Простейший клиент HTTP/1.1 без TLS.
pub struct StdHttp;

impl HttpClient for StdHttp {
    fn get_raw(&self, url: &str, headers: &[(String, String)], max_bytes: usize, timeout_s: u64) -> Result<RawResponse, String> {
        let rest = url.strip_prefix("http://").ok_or_else(|| "HTTPS недоступен во встроенном клиенте".to_string())?;
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let addr = if hostport.contains(':') { hostport.to_string() } else { format!("{}:80", hostport) };
        let sock = addr.to_socket_addrs().map_err(|e| format!("адрес {}: {}", addr, e))?.next().ok_or_else(|| format!("не удалось найти {}", addr))?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(5)).map_err(|e| format!("нет соединения с {}: {}", addr, e))?;
        s.set_read_timeout(Some(Duration::from_secs(timeout_s.max(1)))).ok();
        let mut req = format!("GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: */*\r\n", path, hostport);
        for (k, v) in headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        s.take(max_bytes as u64 + 65536).read_to_end(&mut raw).map_err(|e| format!("чтение ответа: {}", e))?;
        let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("некорректный HTTP-ответ")?;
        let head = String::from_utf8_lossy(&raw[..sep]).to_string();
        let mut body = raw[sep + 4..].to_vec();
        let status: u16 = head.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        let headers: Vec<(String, String)> = head.lines().skip(1).filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string())).collect();
        if head.to_lowercase().contains("transfer-encoding: chunked") {
            body = dechunk(&body);
        }
        Ok(RawResponse { status, headers, body })
    }

    fn request(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<&str>, timeout_s: u64) -> Result<HttpResponse, String> {
        let rest = url.strip_prefix("http://").ok_or_else(|| "HTTPS недоступен во встроенном клиенте".to_string())?;
        let (hostport, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let addr = if hostport.contains(':') { hostport.to_string() } else { format!("{}:80", hostport) };
        let sock = addr
            .to_socket_addrs()
            .map_err(|e| format!("адрес {}: {}", addr, e))?
            .next()
            .ok_or_else(|| format!("не удалось найти {}", addr))?;
        let mut s = TcpStream::connect_timeout(&sock, Duration::from_secs(5)).map_err(|e| format!("нет соединения с {}: {}", addr, e))?;
        s.set_read_timeout(Some(Duration::from_secs(timeout_s.max(1)))).ok();
        s.set_write_timeout(Some(Duration::from_secs(30))).ok();
        let b = body.unwrap_or("");
        let mut req = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: */*\r\n", method, path, hostport);
        for (k, v) in headers {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
        if body.is_some() {
            req.push_str(&format!("Content-Length: {}\r\n", b.len()));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        s.write_all(b.as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).map_err(|e| format!("чтение ответа: {}", e))?;
        let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("некорректный HTTP-ответ")?;
        let head = String::from_utf8_lossy(&raw[..sep]).to_string();
        let mut body_bytes = raw[sep + 4..].to_vec();
        let status: u16 = head.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
        if head.to_lowercase().contains("transfer-encoding: chunked") {
            body_bytes = dechunk(&body_bytes);
        }
        Ok(HttpResponse { status, body: String::from_utf8_lossy(&body_bytes).into_owned() })
    }
}

fn dechunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let Some(le) = b[i..].windows(2).position(|w| w == b"\r\n") else { break };
        let line = String::from_utf8_lossy(&b[i..i + le]).to_string();
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        i += le + 2;
        if size == 0 {
            break;
        }
        let end = (i + size).min(b.len());
        out.extend_from_slice(&b[i..end]);
        i = end + 2;
    }
    out
}
