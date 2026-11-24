//! Ссылки в работах студентов: поиск и чтение содержимого.
//!
//! Студенты часто сдают не файл, а ссылку: Google Документ, Таблицу или
//! Презентацию, файл на Google Диске или Яндекс Диске, репозиторий GitHub,
//! сайт. Если преподаватель включил «Открывать ссылки из работ», содержимое
//! скачивается и передаётся нейросети вместе с работой.
//!
//! Безопасность: адреса в локальной сети и на этом компьютере не открываются
//! (студент мог бы указать адрес LM Studio или роутера); переадресации
//! проверяются на каждом шаге; объём скачивания ограничен.

use super::docx;
use super::http::HttpClient;
use super::json::Json;
use super::text;
use super::zip::ZipArchive;
use std::net::{IpAddr, ToSocketAddrs};

const MAX_BYTES: usize = 15_000_000;
const MAX_TEXT: usize = 30_000;

/// Прочитанная ссылка.
#[derive(Clone, Debug, Default)]
pub struct LinkDoc {
    pub url: String,
    /// что это: «Google Документ», «сайт», «GitHub»…
    pub kind: String,
    pub title: String,
    pub text: String,
    /// почему не удалось открыть (пусто — открыта)
    pub error: String,
}

impl LinkDoc {
    pub fn ok(&self) -> bool {
        self.error.is_empty()
    }
    pub fn to_json(&self, with_text: bool) -> Json {
        let mut o = Json::obj()
            .with("url", self.url.as_str())
            .with("kind", self.kind.as_str())
            .with("title", self.title.as_str())
            .with("chars", self.text.chars().count())
            .with("ok", self.ok())
            .with("error", self.error.as_str());
        if with_text {
            o.set("text", self.text.as_str());
        }
        o
    }
}

// ------------------------------------------------------------------ поиск ссылок

fn trim_url(u: &str) -> String {
    let mut s = u.trim().to_string();
    // хвостовая пунктуация из текста: «см. https://site.ru/page).»
    loop {
        let Some(c) = s.chars().last() else { break };
        let unbalanced = c == ')' && s.matches('(').count() < s.matches(')').count();
        if ".,;:!?»\"'".contains(c) || unbalanced || c == ']' || c == '>' {
            s.pop();
        } else {
            break;
        }
    }
    s
}

/// Ссылки в тексте: http(s)://…, www.…, а также docs.google.com/… без протокола.
pub fn find_in_text(t: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let low = t.to_lowercase();
    let lb = low.as_bytes();
    let mut i = 0;
    while i < lb.len() {
        let rest = &low[i..];
        let start = if rest.starts_with("https://") || rest.starts_with("http://") {
            Some(i)
        } else if (rest.starts_with("www.") || rest.starts_with("docs.google.com/") || rest.starts_with("drive.google.com/") || rest.starts_with("disk.yandex.") || rest.starts_with("github.com/"))
            && (i == 0 || !(lb[i - 1].is_ascii_alphanumeric() || lb[i - 1] == b'/' || lb[i - 1] == b'.' || lb[i - 1] == b'@'))
        {
            Some(i)
        } else {
            None
        };
        if let Some(s) = start {
            // конец ссылки — пробел (и неразрывный), кавычки, «ёлочки», угловые скобки
            let e = t[s..].char_indices().find(|(_, c)| c.is_whitespace() || "<>\"|«»“”„".contains(*c)).map(|(k, _)| s + k).unwrap_or(t.len());
            let mut u = trim_url(&t[s..e]);
            if !u.to_lowercase().starts_with("http") {
                u = format!("https://{}", u);
            }
            if u.len() > 12 && !out.contains(&u) {
                out.push(u);
            }
            i = e.max(i + 1);
            while i < lb.len() && !low.is_char_boundary(i) {
                i += 1;
            }
            continue;
        }
        i += 1;
        while i < lb.len() && !low.is_char_boundary(i) {
            i += 1;
        }
    }
    out
}

/// Ссылки, спрятанные за текстом в документе (гиперссылки Word, PowerPoint,
/// Excel, OpenDocument) и в тексте работы.
pub fn find_in_file(path: &str, bytes: &[u8], visible: &str) -> Vec<String> {
    let mut out = Vec::new();
    if bytes.starts_with(b"PK") {
        if let Ok(z) = ZipArchive::open(bytes) {
            for name in z.names() {
                if name.ends_with(".rels") {
                    let x = z.read_string(name).unwrap_or_default();
                    for tok in docx::xml_iter(&x) {
                        if let docx::Tok::Tag(t) = tok {
                            if t.name.ends_with("Relationship") && t.attr("Type").map(|v| v.ends_with("/hyperlink")).unwrap_or(false) {
                                if let Some(u) = t.attr("Target") {
                                    out.push(docx::decode_entities(&u));
                                }
                            }
                        }
                    }
                } else if name == "content.xml" {
                    let x = z.read_string(name).unwrap_or_default();
                    for tok in docx::xml_iter(&x) {
                        if let docx::Tok::Tag(t) = tok {
                            if t.name == "text:a" {
                                if let Some(u) = t.attr("xlink:href") {
                                    out.push(docx::decode_entities(&u));
                                }
                            }
                        }
                    }
                }
            }
        }
    } else if path.to_lowercase().ends_with(".html") || path.to_lowercase().ends_with(".htm") {
        let s = text::decode_bytes(bytes);
        for tok in docx::xml_iter(&s) {
            if let docx::Tok::Tag(t) = tok {
                if t.name.eq_ignore_ascii_case("a") {
                    if let Some(u) = t.attr("href") {
                        out.push(docx::decode_entities(&u));
                    }
                }
            }
        }
    }
    out.extend(find_in_text(visible));
    let mut res: Vec<String> = Vec::new();
    for u in out {
        let u = trim_url(&u);
        let l = u.to_lowercase();
        if !(l.starts_with("http://") || l.starts_with("https://")) {
            continue; // mailto:, #закладки, file:
        }
        if !res.contains(&u) {
            res.push(u);
        }
    }
    res
}

// ------------------------------------------------------------------ безопасность

fn host_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or("");
    let hp = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hp = hp.rsplit('@').next().unwrap_or(hp);
    if let Some(v6) = hp.strip_prefix('[') {
        return v6.split(']').next().unwrap_or("").to_lowercase();
    }
    hp.split(':').next().unwrap_or("").trim_end_matches('.').to_lowercase()
}

fn private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_broadcast() || o[0] == 0 || (o[0] == 100 && (64..128).contains(&o[1])) || o[0] >= 224
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            v.is_loopback() || v.is_unspecified() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || v.to_ipv4_mapped().map(|x| private_ip(&IpAddr::V4(x))).unwrap_or(false)
        }
    }
}

/// Можно ли открыть адрес: только интернет, не этот компьютер и не локальная сеть.
pub fn allowed(url: &str) -> Result<(), String> {
    if std::env::var("PROVERKA_ALLOW_LOCAL_LINKS").is_ok() {
        return Ok(());
    }
    let l = url.to_lowercase();
    if !(l.starts_with("http://") || l.starts_with("https://")) {
        return Err("поддерживаются только ссылки http и https".into());
    }
    let host = host_of(url);
    if host.is_empty() || host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".lan") || host.ends_with(".internal") || !host.contains('.') && host.parse::<IpAddr>().is_err() {
        return Err("адрес в локальной сети не открывается".into());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if private_ip(&ip) { Err("адрес в локальной сети не открывается".into()) } else { Ok(()) };
    }
    let port = if l.starts_with("https") { 443 } else { 80 };
    if let Ok(addrs) = (host.as_str(), port).to_socket_addrs() {
        if addrs.into_iter().any(|a| private_ip(&a.ip())) {
            return Err("адрес ведёт в локальную сеть и не открывается".into());
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ скачивание

struct Got {
    status: u16,
    ctype: String,
    disposition: String,
    final_url: String,
    body: Vec<u8>,
}

fn header(h: &[(String, String)], name: &str) -> String {
    h.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or_default()
}

fn absolute(base: &str, loc: &str) -> String {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return loc.to_string();
    }
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
    if let Some(rest) = loc.strip_prefix("//") {
        return format!("{}//{}", &base[..scheme_end.saturating_sub(2)], rest);
    }
    if loc.starts_with('/') {
        return format!("{}{}", &base[..host_end], loc);
    }
    let dir_end = base.rfind('/').filter(|&i| i >= host_end).map(|i| i + 1).unwrap_or(base.len());
    format!("{}{}{}", &base[..dir_end], if dir_end == base.len() { "/" } else { "" }, loc)
}

/// Куки на время одного открытия ссылки: многие сайты ставят куку и
/// переадресуют на ту же страницу — без кук это бесконечная переадресация.
#[derive(Default)]
struct Jar(Vec<(String, String, String)>); // (домен, имя, значение)

impl Jar {
    fn take(&mut self, host: &str, headers: &[(String, String)]) {
        for (k, v) in headers {
            if !k.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            let mut parts = v.split(';');
            let Some((name, val)) = parts.next().and_then(|nv| nv.split_once('=')) else { continue };
            let mut domain = host.to_string();
            for a in parts {
                if let Some((ak, av)) = a.split_once('=') {
                    if ak.trim().eq_ignore_ascii_case("domain") {
                        domain = av.trim().trim_start_matches('.').to_lowercase();
                    }
                }
            }
            let name = name.trim().to_string();
            self.0.retain(|(d, n, _)| !(d == &domain && n == &name));
            self.0.push((domain, name, val.trim().to_string()));
        }
    }
    fn header(&self, host: &str) -> Option<String> {
        let c: Vec<String> = self.0.iter().filter(|(d, _, _)| host == d || host.ends_with(&format!(".{}", d))).map(|(_, n, v)| format!("{}={}", n, v)).collect();
        if c.is_empty() { None } else { Some(c.join("; ")) }
    }
}

/// Пауза между запросами к одному сайту: магазины отвечают 429 на частые запросы.
fn polite(host: &str) {
    static LAST: std::sync::Mutex<Vec<(String, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());
    let wait = {
        let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        let w = l.iter().find(|(h, _)| h == host).map(|(_, t)| std::time::Duration::from_millis(1200).saturating_sub(now.duration_since(*t))).unwrap_or_default();
        l.retain(|(h, _)| h != host);
        l.push((host.to_string(), now + w));
        w
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
}

fn get(http: &dyn HttpClient, url: &str, headers: &[(String, String)]) -> Result<Got, String> {
    let mut u = url.to_string();
    let mut jar = Jar::default();
    let mut seen: Vec<String> = Vec::new();
    let mut retried = false;
    let mut hops = 0;
    while hops < 12 {
        allowed(&u)?;
        let host = host_of(&u);
        polite(&host);
        let mut h = headers.to_vec();
        if let Some(c) = jar.header(&host) {
            h.push(("Cookie".into(), c));
        }
        let r = http.get_raw(&u, &h, MAX_BYTES, 40)?;
        jar.take(&host, &r.headers);
        if (300..400).contains(&r.status) {
            let loc = header(&r.headers, "location");
            if loc.is_empty() {
                return Err(format!("переадресация без адреса (код {})", r.status));
            }
            let next = absolute(&u, &loc);
            // по кругу без новых кук — дальше бессмысленно
            let key = format!("{}|{}", next, jar.header(&host_of(&next)).unwrap_or_default());
            if seen.contains(&key) {
                return Err("сайт переадресует по кругу (защита от роботов)".into());
            }
            seen.push(key);
            u = next;
            hops += 1;
            continue;
        }
        if r.status == 429 && !retried {
            // «слишком много запросов»: ждём, сколько просит сайт (не больше 8 с), и пробуем ещё раз
            retried = true;
            let secs = header(&r.headers, "retry-after").trim().parse::<u64>().unwrap_or(3).clamp(1, 8);
            std::thread::sleep(std::time::Duration::from_secs(secs));
            continue;
        }
        return Ok(Got { status: r.status, ctype: header(&r.headers, "content-type").to_lowercase(), disposition: header(&r.headers, "content-disposition"), final_url: u, body: r.body });
    }
    Err("слишком много переадресаций".into())
}

/// Страница-заглушка защиты от роботов вместо содержимого.
pub fn looks_blocked(title: &str, text: &str) -> bool {
    let l = format!("{} {}", title, text.chars().take(3000).collect::<String>()).to_lowercase();
    let short = text.split_whitespace().count() < 25;
    let marks = [
        "captcha", "капча", "qrator", "ddos-guard", "доступ ограничен", "доступ запрещ", "access denied", "just a moment", "checking your browser", "проверка браузера",
        "вы не робот", "не робот", "подтвердите, что вы человек", "antibot", "anti-bot", "enable javascript", "включите javascript", "please wait", "подождите",
        "подозрительн", "too many requests", "forbidden", "smartcaptcha", "showcaptcha",
    ];
    short || (text.split_whitespace().count() < 400 && marks.iter().any(|m| l.contains(m)))
}

/// Начало текста страницы обычно — меню сайта; начинаем с заголовка товара/статьи.
pub fn focus_text(title: &str, text: &str) -> String {
    let head: String = title.split(['|', '—', '–']).next().unwrap_or("").split(" купить").next().unwrap_or("").trim().chars().take(40).collect();
    if head.chars().count() >= 8 {
        let lt = text.to_lowercase();
        // первая строка — сам <title>; ищем заголовок дальше, в теле страницы
        let skip = lt.find('\n').map(|n| n + 1).unwrap_or(0);
        if let Some(i) = lt[skip..].find(&head.to_lowercase()).map(|i| i + skip) {
            // байтовый индекс в нижнем регистре может не совпасть — ищем по исходному тексту
            if let Some(k) = text.char_indices().map(|(k, _)| k).find(|&k| k >= i.min(text.len())) {
                let start = text[..k].char_indices().rev().nth(120).map(|(x, _)| x).unwrap_or(0);
                return text[start..].trim().to_string();
            }
        }
    }
    text.to_string()
}

fn pct(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{:02X}", b));
        }
    }
    o
}

/// Идентификатор Google после «/d/».
fn google_id(url: &str) -> Option<String> {
    if let Some(i) = url.find("/d/") {
        let id: String = url[i + 3..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
        if id.len() > 10 {
            return Some(id);
        }
    }
    let q = url.split('?').nth(1)?;
    q.split('&').find_map(|kv| kv.strip_prefix("id=").map(|v| v.to_string())).filter(|v| v.len() > 10)
}

fn login_wall(g: &Got) -> bool {
    let f = g.final_url.to_lowercase();
    f.contains("accounts.google.com") || f.contains("servicelogin") || f.contains("passport.yandex") || f.contains("/login") && g.ctype.contains("html")
}

fn closed(kind: &str) -> String {
    format!("{}: доступ закрыт — студент не открыл доступ «Все, у кого есть ссылка»", kind)
}

fn title_of(html: &str) -> String {
    let l = html.to_lowercase();
    let Some(a) = l.find("<title") else { return String::new() };
    let Some(b) = l[a..].find('>').map(|x| a + x + 1) else { return String::new() };
    let Some(c) = l[b..].find("</title").map(|x| b + x) else { return String::new() };
    if !html.is_char_boundary(b) || !html.is_char_boundary(c) {
        return String::new();
    }
    docx::decode_entities(html[b..c].trim()).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Текст из скачанного: документ (docx, xlsx, pptx, odt, rtf), веб-страница, текст.
fn text_of(g: &Got, name_hint: &str) -> Result<(String, String), String> {
    let b = &g.body;
    if b.len() > MAX_BYTES {
        return Err("файл больше 15 МБ".into());
    }
    if b.starts_with(b"%PDF") {
        return Err("это PDF — нейросеть получает только текст; скачайте файл и добавьте его к работе (выделите обе и «Объединить в одну работу»)".into());
    }
    // имя файла из заголовка или адреса — для выбора формата
    let mut name = name_hint.to_string();
    if let Some(i) = g.disposition.to_lowercase().find("filename") {
        let v = g.disposition[i..].split('=').nth(1).unwrap_or("").trim_matches(|c| c == '"' || c == '\'' || c == ' ' || c == ';');
        let v = v.rsplit("''").next().unwrap_or(v);
        if !v.is_empty() {
            name = v.to_string();
        }
    }
    if name.is_empty() {
        name = g.final_url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
    }
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    if b.starts_with(b"PK") {
        let z = ZipArchive::open(b).map_err(|e| e.to_string())?;
        let names = z.names();
        let ext = if ["docx", "xlsx", "pptx", "odt", "docm", "xlsm", "ppsx"].contains(&ext.as_str()) {
            ext
        } else if names.iter().any(|n| n.starts_with("word/")) {
            "docx".into()
        } else if names.iter().any(|n| n.starts_with("xl/")) {
            "xlsx".into()
        } else if names.iter().any(|n| n.starts_with("ppt/")) {
            "pptx".into()
        } else if names.contains(&"content.xml") {
            "odt".into()
        } else {
            // архив (например, папка с Яндекс Диска): перечисляем файлы и читаем документы
            let mut out = format!("Архив, файлов: {}\n", names.len());
            for n in names.iter().take(60) {
                out.push_str(&format!("- {}\n", n));
            }
            for n in names.iter().filter(|n| super::core::SUPPORTED.contains(&n.rsplit('.').next().unwrap_or("").to_lowercase().as_str())).take(5) {
                if let Some(bytes) = z.read(n) {
                    if let Ok(ex) = std::panic::catch_unwind(|| docx::extract(n, &bytes)) {
                        let t = ex.visible();
                        if !t.trim().is_empty() {
                            out.push_str(&format!("\n=== {} ===\n{}\n", n, t));
                        }
                    }
                }
            }
            return Ok((out, String::new()));
        };
        let ex = std::panic::catch_unwind(|| docx::extract(&format!("x.{}", ext), b)).map_err(|_| "не удалось разобрать файл".to_string())?;
        return Ok((ex.visible(), name));
    }
    if b.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        let ex = std::panic::catch_unwind(|| docx::extract("x.doc", b)).map_err(|_| "не удалось разобрать файл".to_string())?;
        return Ok((ex.visible(), name));
    }
    if b.starts_with(b"\x89PNG") || b.starts_with(&[0xFF, 0xD8, 0xFF]) || b.starts_with(b"GIF8") {
        return Err("по ссылке картинка — скачайте её и добавьте к работе как фото".into());
    }
    let s = text::decode_bytes(b);
    let head = s.trim_start().chars().take(300).collect::<String>().to_lowercase();
    if g.ctype.contains("html") || head.starts_with("<!doctype html") || head.starts_with("<html") || head.contains("<head") {
        let title = title_of(&s);
        return Ok((docx::from_html(&s).visible(), title));
    }
    if b.starts_with(b"{\\rtf") {
        return Ok((docx::extract("x.rtf", b).visible(), name));
    }
    Ok((text::tidy(&s), name))
}

/// Открывает ссылку и возвращает её текст.
pub fn fetch(http: &dyn HttpClient, url: &str) -> LinkDoc {
    let mut d = LinkDoc { url: url.to_string(), ..Default::default() };
    let r = fetch_inner(http, url, &mut d);
    if let Err(e) = r {
        d.error = e;
        d.text.clear();
    } else if d.text.trim().is_empty() {
        d.error = "по ссылке нет текста".into();
    }
    if d.text.chars().count() > MAX_TEXT {
        d.text = text::clip(&d.text, MAX_TEXT);
    }
    d
}

fn fetch_inner(http: &dyn HttpClient, url: &str, d: &mut LinkDoc) -> Result<(), String> {
    let l = url.to_lowercase();
    let host = host_of(url);
    let none: Vec<(String, String)> = Vec::new();

    // Google Документы, Таблицы, Презентации — через выгрузку в текст
    if host == "docs.google.com" {
        let id = google_id(url).ok_or("не удалось понять ссылку на Google")?;
        let (kind, exp) = if l.contains("/document/") {
            ("Google Документ", format!("https://docs.google.com/document/d/{}/export?format=txt", id))
        } else if l.contains("/spreadsheets/") {
            let gid = url.split(['#', '&', '?']).find_map(|p| p.strip_prefix("gid=")).map(|g| format!("&gid={}", g)).unwrap_or_default();
            ("Google Таблица", format!("https://docs.google.com/spreadsheets/d/{}/export?format=csv{}", id, gid))
        } else if l.contains("/presentation/") {
            ("Google Презентация", format!("https://docs.google.com/presentation/d/{}/export/txt", id))
        } else {
            ("Google Форма", url.to_string())
        };
        d.kind = kind.into();
        let g = get(http, &exp, &none)?;
        if login_wall(&g) || g.status == 401 || g.status == 403 {
            return Err(closed(kind));
        }
        if g.status == 404 {
            return Err(format!("{}: не найден (удалён или ссылка с ошибкой)", kind));
        }
        if g.status >= 400 {
            return Err(format!("{}: сервер ответил {}", kind, g.status));
        }
        let (t, title) = text_of(&g, "")?;
        d.title = title;
        d.text = t;
        return Ok(());
    }

    // файл на Google Диске
    if host == "drive.google.com" {
        d.kind = "Google Диск".into();
        if l.contains("/folders/") {
            return Err("Google Диск: это папка — попросите ссылку на сам файл".into());
        }
        let id = google_id(url).ok_or("не удалось понять ссылку на Google Диск")?;
        let g = get(http, &format!("https://drive.google.com/uc?export=download&id={}", id), &none)?;
        if login_wall(&g) || g.status == 401 || g.status == 403 {
            return Err(closed("Google Диск"));
        }
        if g.status >= 400 {
            return Err(format!("Google Диск: сервер ответил {}", g.status));
        }
        // большой файл: Google спрашивает «скачать без проверки на вирусы?»
        let page = if g.ctype.contains("html") { text::decode_bytes(&g.body) } else { String::new() };
        let g = if page.contains("download-form") || page.contains("confirm=") {
            get(http, &format!("https://drive.usercontent.google.com/download?id={}&export=download&confirm=t", id), &none)?
        } else {
            g
        };
        if g.ctype.contains("html") && text::decode_bytes(&g.body).contains("accounts.google.com") {
            return Err(closed("Google Диск"));
        }
        let (t, title) = text_of(&g, "")?;
        d.title = title;
        d.text = t;
        return Ok(());
    }

    // Яндекс Диск: публичная ссылка → адрес скачивания через открытый API
    if host.starts_with("disk.yandex.") || host == "yadi.sk" || host == "disk.360.yandex.ru" {
        d.kind = "Яндекс Диск".into();
        let api = format!("https://cloud-api.yandex.net/v1/disk/public/resources/download?public_key={}", pct(url));
        let g = get(http, &api, &none)?;
        let j = super::json::parse(&text::decode_bytes(&g.body)).unwrap_or(Json::Null);
        if g.status >= 400 {
            return Err(format!("Яндекс Диск: {}", if g.status == 404 { "файл не найден или доступ закрыт".to_string() } else { j.get("description").str_or("ошибка доступа") }));
        }
        let href = j.get("href").str_or("");
        if href.is_empty() {
            return Err("Яндекс Диск: не удалось получить файл".into());
        }
        let g = get(http, &href, &none)?;
        if g.status >= 400 {
            return Err(format!("Яндекс Диск: сервер ответил {}", g.status));
        }
        let (t, title) = text_of(&g, "")?;
        d.title = title;
        d.text = t;
        return Ok(());
    }

    // GitHub: репозиторий — описание, файлы и README; файл — его текст
    if host == "github.com" || host == "www.github.com" {
        d.kind = "GitHub".into();
        let path: Vec<&str> = url.split("://").nth(1).unwrap_or("").split(['?', '#']).next().unwrap_or("").split('/').skip(1).filter(|x| !x.is_empty()).collect();
        if path.len() >= 2 {
            let (owner, repo) = (path[0], path[1].trim_end_matches(".git"));
            let gh = vec![("Accept".to_string(), "application/vnd.github+json".to_string())];
            if path.len() >= 5 && path[2] == "blob" {
                let raw = format!("https://raw.githubusercontent.com/{}/{}/{}/{}", owner, repo, path[3], path[4..].join("/"));
                let g = get(http, &raw, &none)?;
                if g.status >= 400 {
                    return Err(format!("GitHub: файл недоступен ({})", g.status));
                }
                d.title = path[4..].join("/");
                d.text = text::decode_bytes(&g.body);
                return Ok(());
            }
            let info = get(http, &format!("https://api.github.com/repos/{}/{}", owner, repo), &gh)?;
            if info.status == 404 {
                return Err("GitHub: репозиторий не найден или закрытый".into());
            }
            let j = super::json::parse(&text::decode_bytes(&info.body)).unwrap_or(Json::Null);
            let branch = j.get("default_branch").str_or("main");
            d.title = format!("{}/{}", owner, repo);
            let mut out = format!("Репозиторий {}/{}\nОписание: {}\nЯзык: {}\nСоздан: {}, обновлён: {}\n", owner, repo, j.get("description").str_or("—"), j.get("language").str_or("—"), j.get("created_at").str_or(""), j.get("pushed_at").str_or(""));
            if let Ok(t) = get(http, &format!("https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1", owner, repo, branch), &gh) {
                let tj = super::json::parse(&text::decode_bytes(&t.body)).unwrap_or(Json::Null);
                let files: Vec<String> = tj.get("tree").as_arr().iter().filter(|x| x.get("type").str_or("") == "blob").map(|x| x.get("path").str_or("")).collect();
                if !files.is_empty() {
                    out.push_str(&format!("\nФайлы ({}):\n", files.len()));
                    for f in files.iter().take(150) {
                        out.push_str(&format!("- {}\n", f));
                    }
                }
            }
            let rd = get(http, &format!("https://api.github.com/repos/{}/{}/readme", owner, repo), &[("Accept".to_string(), "application/vnd.github.raw".to_string())]);
            if let Ok(rd) = rd {
                if rd.status < 400 {
                    out.push_str(&format!("\nREADME:\n{}\n", text::decode_bytes(&rd.body)));
                }
            }
            d.text = out;
            return Ok(());
        }
    }

    // остальное: сайт, файл по прямой ссылке
    d.kind = "сайт".into();
    let direct: Result<(String, String, bool), String> = (|| {
        let g = get(http, url, &none)?;
        if g.status == 404 || g.status == 410 {
            return Err("страница не найдена (404)".into());
        }
        if g.status >= 400 {
            return Err(format!("сайт не пустил программу (код {})", g.status));
        }
        let html = g.ctype.contains("html") || g.ctype.is_empty();
        let (t, title) = text_of(&g, "")?;
        Ok((t, title, html))
    })();
    let reason = match direct {
        Ok((t, title, false)) => {
            d.kind = "файл".into();
            d.title = title;
            d.text = t;
            return Ok(());
        }
        Ok((t, title, true)) if !looks_blocked(&title, &t) => {
            d.text = focus_text(&title, &t);
            d.title = title;
            return Ok(());
        }
        Ok(_) => "сайт показал проверку на робота".to_string(),
        Err(e) if e.contains("404") || e.contains("PDF") || e.contains("картинка") || e.contains("15 МБ") || e.contains("локальн") => return Err(e),
        Err(e) => e,
    };
    // магазины и сайты с защитой от роботов, страницы на JavaScript — во встроенном браузере
    match http.render_page(url, 30) {
        Ok(p) if !looks_blocked(&p.title, &p.text) => {
            d.kind = "сайт (открыт браузером)".into();
            d.text = focus_text(&p.title, &p.text);
            d.title = p.title;
            Ok(())
        }
        Ok(_) => Err(format!("{}; во встроенном браузере сайт тоже показал проверку на робота — откройте ссылку сами", reason)),
        Err(e) => Err(format!("{}; встроенный браузер: {}", reason, e)),
    }
}

/// Открытые ссылки на время работы приложения: у многих студентов одни и те же
/// товары и страницы, повторно их не скачиваем.
pub fn fetch_cached(http: &dyn HttpClient, url: &str) -> LinkDoc {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    static CACHE: std::sync::Mutex<Option<HashMap<String, (Instant, LinkDoc)>>> = std::sync::Mutex::new(None);
    {
        let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, d)) = c.as_ref().and_then(|m| m.get(url)) {
            let ttl = if d.ok() { Duration::from_secs(3600) } else { Duration::from_secs(300) };
            if t.elapsed() < ttl {
                return d.clone();
            }
        }
    }
    let d = fetch(http, url);
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(url.to_string(), (Instant::now(), d.clone()));
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links() {
        let t = "Моя работа: https://docs.google.com/document/d/1AbCdEfGhIjKlMnOp/edit?usp=sharing. Также www.example.ru/page) и github.com/lesovikov/pr2, почта a@github.com/x";
        let v = find_in_text(t);
        assert_eq!(v, vec!["https://docs.google.com/document/d/1AbCdEfGhIjKlMnOp/edit?usp=sharing", "https://www.example.ru/page", "https://github.com/lesovikov/pr2"]);
        assert_eq!(find_in_text("сайт https://пример.рф/путь, конец"), vec!["https://пример.рф/путь"]);
        assert_eq!(google_id("https://drive.google.com/open?id=1AbCdEfGhIjKlMnOpQr").as_deref(), Some("1AbCdEfGhIjKlMnOpQr"));
    }

    #[test]
    fn blocks_local() {
        assert!(allowed("http://127.0.0.1:1234/v1/models").is_err());
        assert!(allowed("http://localhost:11434").is_err());
        assert!(allowed("http://192.168.1.1/").is_err());
        assert!(allowed("http://10.0.0.5/").is_err());
        assert!(allowed("http://[::1]/").is_err());
        assert!(allowed("http://router/").is_err());
        assert!(allowed("https://8.8.8.8/").is_ok());
        assert_eq!(absolute("https://a.ru/x/y?z", "/q"), "https://a.ru/q");
        assert_eq!(absolute("https://a.ru/x/y", "w"), "https://a.ru/x/w");
        assert_eq!(absolute("https://a.ru", "//b.ru/c"), "https://b.ru/c");
    }
}

#[cfg(test)]
mod net_tests {
    // запускается вручную: нужен тестовый сервер tools/… на 127.0.0.1:8898
    #[test]
    #[ignore]
    fn shop_cookies_and_429() {
        std::env::set_var("PROVERKA_ALLOW_LOCAL_LINKS", "1");
        let h = super::super::http::StdHttp;
        let d = super::fetch(&h, "http://127.0.0.1:8898/shop/1");
        println!("shop: {:?} {:?} {}", d.error, d.title, d.text);
        assert!(d.ok());
        assert!(d.text.starts_with("Процессор") || d.text.contains("Цена 9 999"));
        let d = super::fetch(&h, "http://127.0.0.1:8898/rl/1");
        println!("rl: {:?} {}", d.error, d.text);
        assert!(d.ok());
        let d = super::fetch(&h, "http://127.0.0.1:8898/loop");
        println!("loop: {:?}", d.error);
        assert!(d.error.contains("по кругу"));
        let d = super::fetch(&h, "http://127.0.0.1:8898/qrator");
        println!("qrator: {:?}", d.error);
        assert!(d.error.contains("401") && d.error.contains("браузер"));
    }
}
