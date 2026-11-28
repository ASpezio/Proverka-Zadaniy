//! Состояние приложения: загруженные задания и работы, результаты, настройки.
//! Единая точка входа `api()` используется интерфейсом (через Tauri),
//! MCP-сервером и тестовым стендом.

use super::assign::{self, AssignmentSet, DocKind, TaskDoc};
use super::detect::{self, Scan};
use super::docx;
use super::http::HttpClient;
use super::journal::{Entry, Journal};
use super::json::{self, Json};
use super::settings;
use super::text;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub type Emitter = Arc<dyn Fn(&str, String) + Send + Sync>;

pub const SUPPORTED: &[&str] = &[
    "docx", "docm", "doc", "odt", "rtf", "txt", "md", "pdf", "html", "htm", // документы
    "xlsx", "xlsm", "xls", "pptx", "ppsx", "pptm", "ppt", // таблицы и презентации
    "jpg", "jpeg", "png", "webp", "bmp", "gif", "heic", "heif", // фото и скриншоты
];

/// Фото больше этого размера уменьшаются в интерфейсе перед отправкой модели.
pub const PAGE_MAX_BYTES: u64 = 1_500_000;

/// Изображение работы (фото, скан, страница PDF без текста), которое
/// распознаёт модель с поддержкой изображений.
#[derive(Clone, Debug, Default)]
pub struct Page {
    /// исходный файл (для страниц PDF — сам PDF)
    pub src: String,
    /// что отправлять модели (исходник или уменьшенная копия)
    pub file: String,
    /// ключ кэша распознанного текста
    pub hash: u64,
    pub size: u64,
    /// готово к отправке (маленькое или уже уменьшено)
    pub ready: bool,
}

#[derive(Clone, Debug)]
pub struct Work {
    pub id: String,
    pub path: String,
    pub name: String,
    pub group: String,
    pub marked: String,
    pub visible: String,
    pub hidden: Vec<String>,
    pub notes: Vec<String>,
    pub images: usize,
    pub needs_pdf: bool,
    pub hash: u64,
    pub size: u64,
    pub assignment: Option<String>,
    /// ФИО студента, если известно точно (например, из Moodle)
    pub student: String,
    /// изображения для распознавания (фото, сканы)
    pub pages: Vec<Page>,
    /// другие файлы, присоединённые к этой работе (страницы-фото, второй файл)
    pub sources: Vec<String>,
    /// текст с изображений уже распознан и добавлен в visible
    pub transcribed: bool,
    /// распознанный текст (хвост visible/marked)
    pub transcript: String,
    /// ссылки в работе (в тексте и за гиперссылками)
    pub urls: Vec<String>,
    /// открытые ссылки (только на время сеанса)
    pub link_docs: Vec<super::links::LinkDoc>,
    /// присоединённые PDF, текст которых ещё читает интерфейс (pdf.js)
    pub pending_pdf: Vec<String>,
    /// другие виды сданного: вставленные в документ таблицы Excel, файлы
    /// из папки студента, которые приложение не читает (draw.io и т.п.)
    pub extra_kinds: Vec<(String, String)>, // (вид, откуда)
    /// книги Excel работы: (имя файла, факты для проверки формул и диаграмм)
    pub xlsx: Vec<(String, super::office::XlsxFacts)>,
    /// срок сдачи (секунды от 1970, UTC; 0 — не задан)
    pub deadline: u64,
    /// когда сдана (секунды от 1970, UTC), запомнено при загрузке работы. Время файла
    /// ненадёжно: перенос на другой компьютер, восстановление из файла экспорта или
    /// открытие в Word делают файл «сданным сейчас» — и работа вдруг «позже срока».
    pub submitted: u64,
}

/// «2026-11-24» из секунд (уже с поправкой на часовой пояс).
pub fn ymd(t: i64) -> String {
    let z = t.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// «4», «4,5»
pub fn fmt_grade(g: f64) -> String {
    if (g - g.round()).abs() < 0.01 {
        format!("{}", g.round() as i64)
    } else {
        // до сотых без лишних нулей: 4,5; 2,7; 2,75
        let t = format!("{:.2}", g);
        t.trim_end_matches('0').trim_end_matches('.').replace('.', ",")
    }
}

/// Приводит оценку к шкале 2–5: целые или с половинками.
pub fn norm_grade(g: f64, half: bool) -> f64 {
    let g = if g.is_finite() { g.clamp(2.0, 5.0) } else { 2.0 };
    if half {
        (g * 2.0).round() / 2.0
    } else {
        g.round()
    }
}

/// Итог правил: оценка, причины, нарушение и подозрение на ИИ, ждущее решения преподавателя.
pub struct Verdict {
    pub grade: f64,
    pub reasons: Vec<String>,
    pub violation: String,
    pub suspect: Json,
    pub review: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub status: String, // pending | queued | running | done | error
    pub phase: String,
    pub message: String,
    pub grade: Option<f64>,
    pub llm: Json,
    pub reasons: Vec<String>,
    pub violation: String, // "" | ai | plagiarism
    pub ai_evidence: Vec<String>,
    pub plag: Vec<(String, f64)>,
    pub scan: Json,
    pub model: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub duration_ms: u64,
    pub override_grade: Option<f64>,
    pub override_comment: String,
    pub checked_at: u64,
    pub log: Vec<String>,
    /// поводы взглянуть преподавателю (расхождение моделей, сомнения насчёт ИИ)
    pub review: Vec<String>,
    /// вопросы для устной защиты {questions:[{q,expect,why}], model, at}
    pub oral: Json,
    /// отзыв студенту (текст)
    pub feedback: String,
    /// решение преподавателя по подозрению на ИИ: "" — не принято, keep — по содержанию, two — «2»
    pub ai_decision: String,
    /// подозрение на ИИ, ждущее решения: {content_grade, signs:[{what, snippet, source}], ...}
    pub ai_suspect: Json,
    /// какие ссылки из работы открывались: [{url, kind, title, chars, ok, error}]
    pub links: Json,
    /// нарушение по правилам и ответу нейросети (до решения преподавателя)
    pub auto_violation: String,
    /// решение преподавателя: "" — как решила проверка, ai, plagiarism, none — нарушения нет
    pub override_violation: String,
    /// отпечаток работы, задания и модели, с которыми работа проверена (для кэша)
    pub fingerprint: String,
    /// результат взят из кэша: когда была настоящая проверка (0 — проверена сейчас)
    pub from_cache: u64,
}

impl Outcome {
    pub fn final_grade(&self) -> Option<f64> {
        // преподаватель отметил ИИ или списывание и сам оценку не ставил — «2»
        let teacher_two = matches!(self.override_violation.as_str(), "ai" | "plagiarism").then_some(2.0);
        self.override_grade.or(teacher_two).or(self.grade)
    }

    /// Нарушение по итогам проверки; итоговое учитывает решение преподавателя.
    pub fn set_auto_violation(&mut self, v: String) {
        self.auto_violation = v;
        self.apply_violation_override();
    }

    pub fn apply_violation_override(&mut self) {
        self.violation = match self.override_violation.as_str() {
            "" => self.auto_violation.clone(),
            "none" => String::new(),
            x => x.to_string(),
        };
    }
    pub fn to_json(&self, work_id: &str, path: &str) -> Json {
        Json::obj()
            .with("work_id", work_id)
            .with("path", path)
            .with("status", self.status.as_str())
            .with("phase", self.phase.as_str())
            .with("message", self.message.as_str())
            .with("grade", self.grade)
            .with("auto_violation", self.auto_violation.as_str())
            .with("override_violation", self.override_violation.as_str())
            .with("final_grade", self.final_grade())
            .with("llm", self.llm.clone())
            .with("reasons", self.reasons.clone())
            .with("violation", self.violation.as_str())
            .with("ai_evidence", self.ai_evidence.clone())
            .with("plag", Json::Arr(self.plag.iter().map(|(n, s)| Json::obj().with("name", n.as_str()).with("score", (*s * 100.0).round())).collect()))
            .with("scan", self.scan.clone())
            .with("model", self.model.as_str())
            .with("tokens_in", self.tokens_in)
            .with("tokens_out", self.tokens_out)
            .with("duration_ms", self.duration_ms)
            .with("override_grade", self.override_grade)
            .with("override_comment", self.override_comment.as_str())
            .with("checked_at", self.checked_at)
            .with("log", self.log.clone())
            .with("review", self.review.clone())
            .with("oral", self.oral.clone())
            .with("feedback", self.feedback.as_str())
            .with("ai_decision", self.ai_decision.as_str())
            .with("ai_suspect", self.ai_suspect.clone())
            .with("links", self.links.clone())
            .with("grade_text", self.final_grade().map(fmt_grade))
            .with("fingerprint", self.fingerprint.as_str())
            .with("from_cache", self.from_cache)
    }
    pub fn from_json(j: &Json) -> Outcome {
        Outcome {
            status: j.get("status").str_or("pending"),
            phase: String::new(),
            message: j.get("message").str_or(""),
            grade: j.get("grade").as_f64(),
            llm: j.get("llm").clone(),
            reasons: j.get("reasons").str_list(),
            violation: j.get("violation").str_or(""),
            ai_evidence: j.get("ai_evidence").str_list(),
            plag: j.get("plag").as_arr().iter().map(|p| (p.get("name").str_or(""), p.get("score").f64_or(0.0) / 100.0)).collect(),
            scan: j.get("scan").clone(),
            model: j.get("model").str_or(""),
            tokens_in: j.get("tokens_in").i64_or(0),
            tokens_out: j.get("tokens_out").i64_or(0),
            duration_ms: j.get("duration_ms").i64_or(0) as u64,
            override_grade: j.get("override_grade").as_f64(),
            override_comment: j.get("override_comment").str_or(""),
            checked_at: j.get("checked_at").i64_or(0) as u64,
            log: vec![],
            review: j.get("review").str_list(),
            oral: j.get("oral").clone(),
            feedback: j.get("feedback").str_or(""),
            ai_decision: j.get("ai_decision").str_or(""),
            ai_suspect: j.get("ai_suspect").clone(),
            links: j.get("links").clone(),
            auto_violation: j.get("auto_violation").as_str().map(|x| x.to_string()).unwrap_or_else(|| j.get("violation").str_or("")),
            override_violation: j.get("override_violation").str_or(""),
            fingerprint: j.get("fingerprint").str_or(""),
            from_cache: j.get("from_cache").i64_or(0) as u64,
        }
    }
}

impl Work {
    /// Когда работа сдана (секунды от 1970, UTC). Файлы из архива Moodle хранят время
    /// сдачи «по часам» сервера (без часового пояса) — переводим по поясу компьютера.
    pub fn submitted_at(&self, data_dir: &Path) -> u64 {
        if self.submitted > 0 {
            return self.submitted;
        }
        self.file_time(data_dir)
    }

    /// Время изменения файла работы (для ответов Moodle — с поправкой на пояс).
    pub fn file_time(&self, data_dir: &Path) -> u64 {
        let Some(t) = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()) else { return 0 };
        if Path::new(&self.path).starts_with(data_dir.join("moodle")) {
            let tz = super::report::TZ_MIN.load(Ordering::Relaxed) * 60;
            (t as i64 - tz).max(0) as u64
        } else {
            t
        }
    }

    /// Сдана позже срока: (когда сдана, срок).
    pub fn late(&self, data_dir: &Path) -> Option<(u64, u64)> {
        if self.deadline == 0 {
            return None;
        }
        let at = self.submitted_at(data_dir);
        (at > self.deadline).then_some((at, self.deadline))
    }

    /// Страницы — картинки, вставленные в сам документ (не фото и не скан).
    pub fn embedded_pictures(&self) -> bool {
        !self.pages.is_empty() && !docx::is_image(&self.path) && ext_of(&self.path) != "pdf" && self.pages.iter().all(|p| p.src == self.path)
    }

    /// Все ссылки работы: гиперссылки из файлов и адреса в тексте (в том числе из PDF и с фото).
    pub fn all_urls(&self) -> Vec<String> {
        let mut v = self.urls.clone();
        for u in super::links::find_in_text(&self.visible) {
            if !v.contains(&u) {
                v.push(u);
            }
        }
        v
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlagInfo {
    /// с кем совпадает: (работа, доля)
    pub list: Vec<(String, f64)>,
    /// ставить «2» за списывание
    pub penalize: bool,
    /// пояснение для ведомости (сколько одинаковых и кто зачтён)
    pub note: String,
    /// сколько работ в группе одинаковых
    pub size: usize,
}

#[derive(Default)]
pub struct Workspace {
    pub tasks: Vec<TaskDoc>,
    pub sets: Vec<AssignmentSet>,
    pub works: Vec<Work>,
    pub outcomes: HashMap<String, Outcome>,
    pub plag: Vec<(String, String, f64)>,
    /// одинаковые работы напарников (одна работа пары, сданная двумя студентами)
    pub team_dups: Vec<(String, String)>,
    /// совпадения с архивом работ прошлых лет: id работы → [(чья работа, доля)]
    pub archive_hits: HashMap<String, Vec<(String, f64)>>,
    /// очередь, поставленная на паузу: продолжится по «Продолжить» (в том числе после перезапуска)
    pub paused: Vec<String>,
    /// очередь на паузе была «без кэша» (Проверить заново без кэша)
    pub paused_fresh: bool,
}

pub struct Core {
    pub data_dir: PathBuf,
    pub http: Arc<dyn HttpClient>,
    pub emit: Emitter,
    pub ws: Mutex<Workspace>,
    pub settings: Mutex<Json>,
    pub running: AtomicBool,
    pub cancel: AtomicBool,
    /// «Пауза»: новые работы из очереди не берутся, начатые доделываются
    pub pause: AtomicBool,
    /// текущая проверка идёт без кэша результатов («проверить заново»)
    pub no_cache: AtomicBool,
    /// очередь идущей проверки: в неё можно добавлять и убирать работы на ходу
    pub queue: Mutex<Vec<String>>,
    /// работы, снятые с проверки, пока нейросеть их проверяла
    pub dropped: Mutex<std::collections::HashSet<String>>,
    /// сколько работ в текущей проверке (меняется при добавлении и снятии)
    pub run_total: std::sync::atomic::AtomicUsize,
    /// токены текущей проверки: отправлено модели / получено от неё
    pub run_tokens_in: std::sync::atomic::AtomicI64,
    pub run_tokens_out: std::sync::atomic::AtomicI64,
    next_id: AtomicU64,
    restoring: AtomicBool,
    pub journal: Mutex<Journal>,
}

pub fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn fnv(b: &[u8]) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for x in b {
        h ^= *x as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

pub fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.to_string())
}

fn parent_name(p: &str) -> String {
    Path::new(p).parent().and_then(|x| x.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// «Лесовиков_стр2», «Лесовиков (3)», «Лесовиков-1» → «лесовиков»; «IMG_1234», «photo», «1» → "" (безымянное фото).
fn page_stem(p: &str) -> String {
    let stem = Path::new(p).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut t = stem.trim().to_string();
    loop {
        let before = t.clone();
        t = t.trim_end_matches(|c: char| c.is_ascii_digit() || " _-()[].,№#".contains(c)).to_string();
        for suf in ["страница", "стр", "лист", "page", "pg", "p", "фото", "photo", "img", "скрин", "screenshot"] {
            if t.ends_with(suf) && t.len() > suf.len() {
                t = t[..t.len() - suf.len()].to_string();
            }
        }
        if t == before {
            break;
        }
    }
    let t = t.trim().to_string();
    let generic = ["img", "image", "photo", "фото", "scan", "скан", "screenshot", "снимок", "скриншот", "whatsapp", "telegram", "pxl", "dsc", "dcim", "page", "страница", "стр", "без названия", "untitled", "изображение"];
    if t.chars().filter(|c| c.is_alphabetic()).count() < 2 || generic.iter().any(|g| t.starts_with(g)) {
        String::new()
    } else {
        t
    }
}

/// Добавляет распознанный с изображений текст в конец работы.
pub fn apply_transcript(w: &mut Work, texts: &[String]) {
    let n = texts.len();
    let mut t = String::new();
    for (i, x) in texts.iter().enumerate() {
        let src = w.pages.get(i).map(|p| p.src.clone()).unwrap_or_default();
        let label = if ext_of(&src) == "pdf" {
            format!("Страница {} из {} (скан PDF)", i + 1, n)
        } else if src == w.path && !docx::is_image(&src) {
            format!("Рисунок {} из {} в документе", i + 1, n)
        } else {
            format!("Изображение {} из {}: «{}»", i + 1, n, file_name(&src))
        };
        if n > 1 || !w.visible.trim().is_empty() {
            t.push_str(&format!("\n\n[{}]\n", label));
        }
        t.push_str(x.trim());
    }
    if w.visible.trim().is_empty() {
        t = t.trim_start().to_string();
    }
    w.visible.push_str(&t);
    w.marked.push_str(&t);
    w.transcript = t;
    w.transcribed = true;
    w.notes.retain(|x| !x.starts_with("Текст распознан") && !x.starts_with("Картинки внутри документа ("));
    w.notes.retain(|x| !x.starts_with("Картинки внутри документа не прочитаны"));
    w.notes.push(if w.embedded_pictures() {
        format!("Картинки внутри документа ({} шт.) описаны моделью и добавлены к тексту работы", n)
    } else {
        format!("Текст распознан с изображений ({} шт.) моделью — при спорной оценке сверьте с оригиналом", n)
    });
}

/// Таблицы, презентации и документы, вставленные внутрь файла Office (word/embeddings…).
pub fn embedded_kinds(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !bytes.starts_with(b"PK") {
        return out;
    }
    if let Ok(z) = super::zip::ZipArchive::open(bytes) {
        // схема, нарисованная фигурами, надписями, соединительными линиями или SmartArt, —
        // это тоже «схема», даже если файла draw.io нет
        let parts: Vec<String> = z
            .names()
            .into_iter()
            .filter(|n| *n == "word/document.xml" || (n.starts_with("ppt/slides/slide") && n.ends_with(".xml")) || (n.starts_with("xl/drawings/") && n.ends_with(".xml")))
            .map(|n| n.to_string())
            .collect();
        let shapes = parts.iter().any(|n| {
            let x = z.read_string(n).unwrap_or_default();
            ["<wps:wsp", "<wpg:wgp", "<wpc:wpc", "<v:rect", "<v:oval", "<v:line", "<v:group", "<v:roundrect", "<p:cxnSp", "<xdr:sp", "<xdr:cxnSp", "dgm:relIds"]
                .iter()
                .any(|m| x.contains(m))
        });
        if shapes {
            out.push(("drawio".to_string(), "фигуры или SmartArt в документе".to_string()));
        }
        for n in z.names() {
            if n.contains("/embeddings/") {
                let k = detect::file_kind(n);
                if ["word", "excel", "powerpoint"].contains(&k) && !out.iter().any(|(x, _): &(String, String)| x == k) {
                    out.push((k.to_string(), "вставлен в документ".to_string()));
                }
            }
        }
    }
    out
}

/// Все файлы в папке студента Moodle (в том числе те, что приложение не читает).
pub fn moodle_folder_files(path: &str) -> Vec<String> {
    let p = Path::new(path);
    let Some(dir) = p.parent() else { return Vec::new() };
    let dname = dir.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    if !dname.contains("_assignsubmission_") {
        return Vec::new();
    }
    std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|x| x.is_file() && x.as_path() != p).map(|x| x.to_string_lossy().to_string()).collect())
        .unwrap_or_default()
}

/// Другие файлы студента в его папке Moodle («Фамилия Имя_123_assignsubmission_file»).
pub fn moodle_siblings(path: &str) -> Vec<String> {
    let p = Path::new(path);
    let Some(dir) = p.parent() else { return Vec::new() };
    let dname = dir.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    if !dname.contains("_assignsubmission_") || super::moodle::parse_name(&dname).is_none() {
        return Vec::new();
    }
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|x| x.is_file() && x.as_path() != p && SUPPORTED.contains(&ext_of(&x.to_string_lossy()).as_str()))
                .map(|x| x.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|x| natural_key(&file_name(x)));
    out
}

/// Делит найденные файлы на работы и присоединяемые к ним фото.
fn group_pages(files: &[String], roots: &[String]) -> (Vec<String>, HashMap<String, Vec<String>>) {
    let mut dirs: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for f in files {
        let d = Path::new(f).parent().map(|x| x.to_path_buf()).unwrap_or_default();
        match dirs.iter_mut().find(|x| x.0 == d) {
            Some(x) => x.1.push(f.clone()),
            None => dirs.push((d, vec![f.clone()])),
        }
    }
    let mut mains = Vec::new();
    let mut attach: HashMap<String, Vec<String>> = HashMap::new();
    for (dir, list) in dirs {
        let (imgs, docs_all): (Vec<String>, Vec<String>) = list.into_iter().partition(|f| docx::is_image(f));
        // файлы одной работы с одинаковым именем: «Лесовиков_В5.docx» и «Лесовиков_В5.xlsx»
        let mut by_stem: Vec<(String, Vec<String>)> = Vec::new();
        for d in docs_all {
            let st = Path::new(&d).file_stem().map(|x| x.to_string_lossy().trim().to_lowercase()).unwrap_or_default();
            match by_stem.iter_mut().find(|g| g.0 == st) {
                Some(g) => g.1.push(d),
                None => by_stem.push((st, vec![d])),
            }
        }
        let mut docs: Vec<String> = Vec::new();
        for (_, mut g) in by_stem {
            if g.len() > 1 {
                let rank = |f: &String| match detect::file_kind(f) {
                    "word" => 0,
                    "pdf" => 1,
                    "powerpoint" => 2,
                    "excel" => 3,
                    _ => 4,
                };
                g.sort_by_key(|f| rank(f));
                let main = g.remove(0);
                attach.entry(main.clone()).or_default().extend(g);
                docs.push(main);
            } else {
                docs.extend(g);
            }
        }
        if imgs.is_empty() {
            mains.extend(docs);
            continue;
        }
        // папка студента внутри выбранной папки (а не сама выбранная папка)
        let sub = roots.iter().any(|r| {
            let r = Path::new(r);
            r.is_dir() && dir.starts_with(r) && dir != r
        });
        if docs.len() == 1 {
            attach.entry(docs[0].clone()).or_default().extend(imgs);
            mains.extend(docs);
            continue;
        }
        if docs.is_empty() && sub {
            let mut it = imgs.into_iter();
            if let Some(first) = it.next() {
                let rest: Vec<String> = it.collect();
                if !rest.is_empty() {
                    attach.insert(first.clone(), rest);
                }
                mains.push(first);
            }
            continue;
        }
        let doc_stems: Vec<(String, String)> = docs.iter().map(|d| (page_stem(d), d.clone())).collect();
        mains.extend(docs);
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for im in imgs {
            let st = page_stem(&im);
            if !st.is_empty() {
                if let Some((_, d)) = doc_stems.iter().find(|(s, _)| *s == st) {
                    attach.entry(d.clone()).or_default().push(im);
                    continue;
                }
                if let Some(g) = groups.iter_mut().find(|g| g.0 == st) {
                    g.1.push(im);
                    continue;
                }
            }
            groups.push((st, vec![im]));
        }
        for (_, g) in groups {
            let mut it = g.into_iter();
            if let Some(first) = it.next() {
                let rest: Vec<String> = it.collect();
                if !rest.is_empty() {
                    attach.insert(first.clone(), rest);
                }
                mains.push(first);
            }
        }
    }
    (mains, attach)
}

pub fn ext_of(p: &str) -> String {
    Path::new(p).extension().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Каталог данных: %APPDATA%\proverka, иначе ~/.config/proverka.
/// Данные из старой папки `ru.pspu.proverka` при первом запуске переносятся сюда.
pub fn default_data_dir() -> PathBuf {
    if let Some(p) = portable_dir() {
        return p;
    }
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join("proverka");
    migrate_data_dir(&base.join("ru.pspu.proverka"), &dir);
    // папку данных можно перенести (Настройки → «О программе» → «Сменить папку»):
    // в стандартной папке остаётся только указатель `moved_to.txt`
    if let Some(p) = moved_dir(&dir) {
        return p;
    }
    dir
}

/// Стандартная папка данных (без учёта переноса и переносного режима).
pub fn standard_data_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("proverka")
}

/// Куда перенесены данные: путь из `moved_to.txt` стандартной папки, если та папка доступна.
pub fn moved_dir(std_dir: &Path) -> Option<PathBuf> {
    let t = std::fs::read_to_string(std_dir.join("moved_to.txt")).ok()?;
    let p = PathBuf::from(t.trim());
    if t.trim().is_empty() || p == std_dir {
        return None;
    }
    // флешка не вставлена или диск отключён — работаем в стандартной папке, указатель не трогаем
    if std::fs::create_dir_all(&p).is_err() || !p.is_dir() {
        return None;
    }
    Some(p)
}

/// Переносной режим: данные лежат рядом с программой, в папке «Данные» (например, на флешке).
/// Включается файлом `portable.txt` рядом с exe или уже существующей «Данные/settings.json».
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let d = dir.join("Данные");
    if dir.join("portable.txt").exists() || d.join("settings.json").exists() {
        let _ = std::fs::create_dir_all(&d);
        Some(d)
    } else {
        None
    }
}

/// Папка данных переехала (флешка получила другую букву диска, данные перенесли в «Данные»):
/// пути в сохранённом JSON, указывающие в прежнюю папку, — в текущую. Прежняя запоминается в `data_dir.txt`.
pub fn rewrite_moved_dir(text: String, data_dir: &Path) -> String {
    let Ok(old) = std::fs::read_to_string(data_dir.join("data_dir.txt")) else { return text };
    let old = old.trim().to_string();
    let new = data_dir.to_string_lossy().to_string();
    if old.is_empty() || old == new {
        return text;
    }
    let (oj, nj) = (Json::Str(old).to_string(), Json::Str(new).to_string());
    text.replace(oj.trim_matches('"'), nj.trim_matches('"'))
}

/// Пути в сохранённом JSON (сессия, настройки), указывающие в старую папку данных, — в новую.
pub fn rewrite_old_dir(text: String, data_dir: &Path) -> String {
    let text = rewrite_moved_dir(text, data_dir);
    let Some(base) = data_dir.parent() else { return text };
    let old = base.join("ru.pspu.proverka").to_string_lossy().to_string();
    let new = data_dir.to_string_lossy().to_string();
    if old == new || Path::new(&old).join("session.json").exists() {
        return text;
    }
    let (oj, nj) = (Json::Str(old).to_string(), Json::Str(new).to_string());
    text.replace(oj.trim_matches('"'), nj.trim_matches('"'))
}

/// Перенос данных из старой папки: переименованием, а если не вышло — копированием.
pub fn migrate_data_dir(old: &Path, new: &Path) {
    if !old.is_dir() || new.join("settings.json").exists() || new.join("session.json").exists() || new.join("journal.json").exists() {
        return;
    }
    if !new.exists() && std::fs::rename(old, new).is_ok() {
        return;
    }
    fn copy_dir(a: &Path, b: &Path) {
        let _ = std::fs::create_dir_all(b);
        if let Ok(rd) = std::fs::read_dir(a) {
            for e in rd.flatten() {
                let p = e.path();
                let t = b.join(e.file_name());
                if p.is_dir() {
                    copy_dir(&p, &t);
                } else if !t.exists() {
                    let _ = std::fs::copy(&p, &t);
                }
            }
        }
    }
    copy_dir(old, new);
    let _ = std::fs::write(old.join("ПЕРЕНЕСЕНО.txt"), format!("Данные приложения «Проверка заданий» теперь лежат в {}. Эту папку можно удалить.", new.display()));
}

impl Core {
    pub fn new(data_dir: PathBuf, http: Arc<dyn HttpClient>, emit: Emitter) -> Arc<Core> {
        let _ = std::fs::create_dir_all(&data_dir);
        let saved = std::fs::read_to_string(data_dir.join("settings.json")).ok().map(|t| rewrite_old_dir(t, &data_dir)).and_then(|s| json::parse(&s).ok()).unwrap_or(Json::Null);
        // в сохранённом списке направлений ещё нет школы — добавляем один раз
        let saved = {
            let mut sv = saved;
            if !sv.is_null() && !sv.get("programs_school").bool_or(false) {
                let mut list = sv.get("programs").str_list();
                if !list.is_empty() {
                    for (i, x) in ["Школа (1–4 класс)", "Школа (5–9 класс)", "Школа (10–11 класс)"].iter().enumerate() {
                        if !list.iter().any(|p| p == x) {
                            list.insert(i, x.to_string());
                        }
                    }
                    sv.set("programs", list);
                }
                sv.set("programs_school", true);
            }
            // оценки в журнал теперь записываются только кнопкой — один раз выключаем автозапись
            if !sv.is_null() && !sv.get("journal_manual").bool_or(false) {
                sv.set("journal_auto", false);
                sv.set("journal_manual", true);
            }
            sv
        };
        let st = settings::merge(&settings::defaults(), &saved);
        let data_dir_for_journal = data_dir.clone();
        let core = Arc::new(Core {
            data_dir,
            http,
            emit,
            ws: Mutex::new(Workspace::default()),
            settings: Mutex::new(st),
            running: AtomicBool::new(false),
            queue: Mutex::new(Vec::new()),
            dropped: Mutex::new(std::collections::HashSet::new()),
            run_total: std::sync::atomic::AtomicUsize::new(0),
            run_tokens_in: std::sync::atomic::AtomicI64::new(0),
            run_tokens_out: std::sync::atomic::AtomicI64::new(0),
            cancel: AtomicBool::new(false),
            pause: AtomicBool::new(false),
            no_cache: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            restoring: AtomicBool::new(true),
            journal: Mutex::new(Journal::load(&data_dir_for_journal)),
        });
        core.restore_session();
        core.restoring.store(false, Ordering::SeqCst);
        // где лежат данные сейчас — чтобы после переезда (другая буква флешки) поправить пути
        if std::fs::read_to_string(core.data_dir.join("data_dir.txt")).map(|t| t.trim() != core.data_dir.to_string_lossy()).unwrap_or(true) {
            core.save_session();
            core.save_settings();
            let _ = std::fs::write(core.data_dir.join("data_dir.txt"), core.data_dir.to_string_lossy().as_bytes());
        }
        // правила оценки могли поменяться (новая версия, присоединённые файлы) —
        // пересчитываем итог уже проверенных работ по сохранённому ответу модели
        core.refinalize_all();
        core.backup_daily();
        // «Отменить» живёт до перезапуска: после него удалённые файлы Moodle уже не нужны
        let _ = std::fs::remove_dir_all(core.data_dir.join("undo"));
        core.cleanup_moodle();
        core
    }

    // -------------------------------------------------------------- резервные копии и «Отменить»

    const SAVED_FILES: [&'static str; 3] = ["session.json", "journal.json", "settings.json"];

    /// Раз в день — копия сессии, журнала и настроек в `backups/ГГГГ-ММ-ДД`; хранятся последние 10.
    pub fn backup_daily(&self) {
        let day = ymd(now_s() as i64 + super::report::TZ_MIN.load(Ordering::Relaxed) * 60);
        let root = self.data_dir.join("backups");
        let dir = root.join(&day);
        if !dir.exists() && Self::SAVED_FILES.iter().any(|f| self.data_dir.join(f).exists()) {
            let _ = std::fs::create_dir_all(&dir);
            for f in Self::SAVED_FILES {
                let _ = std::fs::copy(self.data_dir.join(f), dir.join(f));
            }
        }
        let mut days: Vec<PathBuf> = std::fs::read_dir(&root).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
        days.sort();
        while days.len() > 10 {
            let _ = std::fs::remove_dir_all(days.remove(0));
        }
        let _ = self.backup_external(false);
    }

    /// Копия в свою папку (Яндекс.Диск, флешка): раз в день — файл экспорта
    /// «Проверка заданий ГГГГ-ММ-ДД.proverka» (настройки без ключей, работы, журнал, кэш).
    /// Восстанавливается обычным «Импортом». Хранятся последние `keep` файлов.
    pub fn backup_external(&self, force: bool) -> Result<Option<String>, String> {
        let b = self.settings().get("backup").clone();
        let dir = b.get("dir").str_or("");
        if dir.trim().is_empty() {
            return Ok(None);
        }
        let dir = PathBuf::from(dir.trim());
        if !dir.is_dir() {
            return Err(format!("Папка для копий недоступна: {}", dir.display()));
        }
        let day = ymd(now_s() as i64 + super::report::TZ_MIN.load(Ordering::Relaxed) * 60);
        let dest = dir.join(format!("Проверка заданий {}.proverka", day));
        if dest.exists() && !force {
            return Ok(None);
        }
        self.save_session();
        self.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
        let parts = super::transfer::Parts { settings: true, keys: false, session: true, journal: true, results: true, looks: true, files: b.get("files").bool_or(false) };
        super::transfer::export(self, &dest.to_string_lossy(), parts)?;
        let keep = b.get("keep").f64_or(10.0).max(1.0) as usize;
        let mut old: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.file_name().map(|n| { let n = n.to_string_lossy(); n.starts_with("Проверка заданий ") && n.ends_with(".proverka") }).unwrap_or(false)).collect())
            .unwrap_or_default();
        old.sort();
        while old.len() > keep {
            let _ = std::fs::remove_file(old.remove(0));
        }
        self.log(&format!("Резервная копия: {}", dest.display()));
        Ok(Some(dest.to_string_lossy().to_string()))
    }

    /// Список резервных копий: (папка, сколько оценок в журнале, сколько работ в сессии).
    pub fn backups(&self) -> Json {
        let root = self.data_dir.join("backups");
        let mut days: Vec<PathBuf> = std::fs::read_dir(&root).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
        days.sort();
        days.reverse();
        let count = |p: &Path, key: &str| std::fs::read_to_string(p).ok().and_then(|t| json::parse(&t).ok()).map(|j| j.get(key).as_arr().len()).unwrap_or(0);
        Json::Arr(
            days.iter()
                .map(|d| {
                    let name = d.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    Json::obj().with("name", name.as_str()).with("journal", count(&d.join("journal.json"), "entries")).with("works", count(&d.join("session.json"), "works"))
                })
                .collect(),
        )
    }

    /// Перечитать сессию, журнал и настройки с диска (после восстановления копии или «Отменить»).
    pub fn reload_from_disk(&self) {
        let saved = std::fs::read_to_string(self.data_dir.join("settings.json")).ok().and_then(|s| json::parse(&s).ok()).unwrap_or(Json::Null);
        *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings::merge(&settings::defaults(), &saved);
        *self.journal.lock().unwrap_or_else(|e| e.into_inner()) = Journal::load(&self.data_dir);
        *self.ws.lock().unwrap_or_else(|e| e.into_inner()) = Workspace::default();
        self.restoring.store(true, Ordering::SeqCst);
        self.restore_session();
        self.restoring.store(false, Ordering::SeqCst);
        self.compute_plagiarism();
        self.refinalize_all();
    }

    /// Восстановить резервную копию дня `name`. Текущее состояние сначала запоминается для «Отменить».
    pub fn restore_backup(&self, name: &str) -> Result<(), String> {
        if name.contains(['/', '\\']) || name.contains("..") {
            return Err("неверное имя копии".into());
        }
        let dir = self.data_dir.join("backups").join(name);
        if !dir.is_dir() {
            return Err("копия не найдена".into());
        }
        self.snapshot(&format!("восстановление копии от {}", name));
        for f in Self::SAVED_FILES {
            if dir.join(f).exists() {
                std::fs::copy(dir.join(f), self.data_dir.join(f)).map_err(|e| format!("не удалось восстановить {}: {}", f, e))?;
            }
        }
        self.reload_from_disk();
        Ok(())
    }

    /// Снимок перед удалением: «Отменить» возвращает именно это состояние.
    pub fn snapshot(&self, what: &str) {
        self.save_session();
        let dir = self.data_dir.join("undo");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        self.journal.lock().unwrap_or_else(|e| e.into_inner()).save();
        for f in Self::SAVED_FILES {
            let _ = std::fs::copy(self.data_dir.join(f), dir.join(f));
        }
        let _ = std::fs::write(dir.join("what.txt"), what);
    }

    /// Удалить из папки `moodle` файлы, которые больше не нужны ни одной работе
    /// (и тем, что вернёт «Отменить»), а также опустевшие папки.
    pub fn cleanup_moodle(&self) {
        self.cleanup_files();
    }

    /// Убрать из папки данных всё, на что больше ничего не ссылается: распакованные ответы
    /// Moodle, фото и страницы сканов (с распознанным текстом), импортированные файлы,
    /// задания с фото — и опустевшие папки. Файлы состояния для «Отменить» сохраняются.
    pub fn cleanup_files(&self) {
        let mut keep: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            for w in &ws.works {
                keep.insert(PathBuf::from(&w.path));
                hashes.insert(format!("{:016x}", w.hash));
                for s in &w.sources {
                    keep.insert(PathBuf::from(s));
                }
                for pg in &w.pages {
                    keep.insert(PathBuf::from(&pg.file));
                    keep.insert(PathBuf::from(&pg.src));
                    hashes.insert(format!("{:016x}", pg.hash));
                }
            }
            for t in &ws.tasks {
                keep.insert(PathBuf::from(&t.path));
            }
        }
        // состояние до последнего удаления («Отменить»): его файлы и страницы не трогаем
        if let Some(doc) = std::fs::read_to_string(self.data_dir.join("undo").join("session.json")).ok().and_then(|t| json::parse(&t).ok()) {
            let mut files: Vec<String> = Vec::new();
            for w in doc.get("works").as_arr() {
                files.push(w.get("path").str_or(""));
                files.extend(w.get("sources").str_list());
            }
            for t in doc.get("tasks").as_arr() {
                files.push(t.get("path").str_or(""));
            }
            for f in files {
                if let Ok(b) = std::fs::read(&f) {
                    hashes.insert(format!("{:016x}", fnv(&b)));
                }
                keep.insert(PathBuf::from(f));
            }
        }
        fn walk(dir: &Path, keep: &dyn Fn(&Path) -> bool) -> bool {
            // true — папка опустела
            let Ok(rd) = std::fs::read_dir(dir) else { return false };
            let mut empty = true;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if walk(&p, keep) {
                        let _ = std::fs::remove_dir(&p);
                    } else {
                        empty = false;
                    }
                } else if keep(&p) {
                    empty = false;
                } else if std::fs::remove_file(&p).is_err() {
                    empty = false;
                }
            }
            empty
        }
        let by_path = |p: &Path| keep.contains(p);
        for sub in ["moodle", "imported", "tasks_text"] {
            let root = self.data_dir.join(sub);
            if root.is_dir() && walk(&root, &by_path) {
                let _ = std::fs::remove_dir(&root);
            }
        }
        // фото и страницы: имя начинается с хэша исходного файла или страницы
        let pages = self.data_dir.join("pages");
        if pages.is_dir() {
            let by_hash = |p: &Path| {
                keep.contains(p) || p.file_name().map(|n| n.to_string_lossy().chars().take(16).collect::<String>()).map(|h| hashes.contains(&h)).unwrap_or(false)
            };
            walk(&pages, &by_hash);
        }
    }

    /// Вернуть состояние до последнего удаления. Возвращает, что было отменено.
    pub fn undo(&self) -> Result<String, String> {
        let dir = self.data_dir.join("undo");
        let what = std::fs::read_to_string(dir.join("what.txt")).map_err(|_| "Отменять нечего".to_string())?;
        for f in Self::SAVED_FILES {
            if dir.join(f).exists() {
                let _ = std::fs::copy(dir.join(f), self.data_dir.join(f));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        self.reload_from_disk();
        Ok(what)
    }

    pub fn settings(&self) -> Json {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn new_id(&self, prefix: &str) -> String {
        format!("{}{}", prefix, self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    pub fn log(&self, line: &str) {
        (self.emit)("log", Json::obj().with("t", now_s()).with("line", line).to_string());
    }

    // -------------------------------------------------------------- загрузка

    /// Раскрывает папки в список файлов поддерживаемых форматов.
    pub fn expand(paths: &[String]) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, Option<String>)>) {
            if depth > 6 {
                return;
            }
            let Ok(rd) = std::fs::read_dir(dir) else { return };
            let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if name.starts_with("~$") || name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    if skip_dir(&name) {
                        continue;
                    }
                    walk(&p, depth + 1, out);
                } else if SUPPORTED.contains(&ext_of(&p.to_string_lossy()).as_str()) {
                    out.push((p.to_string_lossy().to_string(), None));
                }
            }
        }
        for p in paths {
            let pb = PathBuf::from(p);
            if pb.is_dir() {
                walk(&pb, 0, &mut out);
            } else if pb.is_file() {
                out.push((p.clone(), None));
            }
        }
        out
    }

}

/// Папки, в которых не бывает заданий и работ: лекции, тесты, справочные материалы,
/// а также отмеченные преподавателем как неиспользуемые.
fn skip_dir(name: &str) -> bool {
    let l = name.to_lowercase().replace('ё', "е");
    // «-ПР4…» — так преподаватель помечает отключённую работу
    l.starts_with('-')
        || ["банк информации", "лекци", "тестирован", "литератур", "не использует", "архив", "старое"].iter().any(|k| l.contains(k))
        || l == "old"
}

/// Разбор одного файла; паника в разборщике не роняет загрузку остальных.
pub fn extract_safe(p: &str, bytes: &[u8]) -> Result<docx::Extracted, String> {
    let p2 = p.to_string();
    let b2 = bytes.to_vec();
    std::panic::catch_unwind(move || docx::extract(&p2, &b2)).map_err(|e| {
        let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
        format!("{}: не удалось прочитать файл{}", file_name(p), if msg.is_empty() { String::new() } else { format!(" ({})", msg) })
    })
}

/// Направление, если оно есть в пути к файлу: «…\СПО-9\Задания\…» → «СПО-9».
pub fn program_in_path(path: &str, programs: &[String]) -> Option<String> {
    let comps: Vec<String> = Path::new(path).components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
    programs.iter().find(|p| {
        let pl = p.to_lowercase();
        !pl.is_empty() && comps.iter().any(|c| c == &pl || c.starts_with(&format!("{} ", pl)) || c.starts_with(&format!("{}_", pl)))
    }).cloned()
}

/// Метаданные файла задания при восстановлении сессии: (вид, для кого, код).
pub type TaskMeta = HashMap<String, (String, String, String)>;

impl Core {
    pub fn add_tasks(&self, paths: &[String], kinds: Option<&TaskMeta>, scope: &str) -> Vec<String> {
        // выбрали папку дисциплины целиком («СПО-9»), а в ней есть «Задания» —
        // берём только её: программы, графики и прочие файлы в корне — не задания
        let paths: Vec<String> = paths
            .iter()
            .map(|p| {
                let pb = PathBuf::from(p);
                if pb.is_dir() {
                    if let Ok(rd) = std::fs::read_dir(&pb) {
                        for e in rd.filter_map(|e| e.ok()) {
                            let n = e.file_name().to_string_lossy().to_lowercase();
                            if e.path().is_dir() && (n == "задания" || n == "практические работы" || n == "практические") {
                                return e.path().to_string_lossy().to_string();
                            }
                        }
                    }
                }
                p.clone()
            })
            .collect();
        // фото и скриншоты в папке заданий — иллюстрации, а не задания
        let files: Vec<(String, Option<String>)> = Core::expand(&paths).into_iter().filter(|(p, _)| !docx::is_image(p) && !["heic", "heif"].contains(&ext_of(p).as_str())).collect();
        let mut errors = Vec::new();
        let programs = self.settings().get("programs").str_list();
        let (known_paths, known_stems, mut known_hashes) = {
            let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            (
                ws.tasks.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                ws.tasks.iter().filter(|t| ext_of(&t.path) != "pdf").map(|t| Path::new(&t.path).with_extension("").to_string_lossy().to_lowercase()).collect::<Vec<_>>(),
                ws.tasks.iter().filter_map(|t| std::fs::read(&t.path).ok().map(|b| (fnv(&b), b.len()))).collect::<Vec<_>>(),
            )
        };
        // pdf пропускаем, если рядом есть docx/odt с тем же именем
        let stems: Vec<String> = files
            .iter()
            .filter(|(p, _)| ext_of(p) != "pdf")
            .map(|(p, _)| Path::new(p).with_extension("").to_string_lossy().to_lowercase())
            .chain(known_stems)
            .collect();
        let mut fresh: Vec<TaskDoc> = Vec::new();
        let mut dupes = 0;
        for (p, _) in files {
            if known_paths.contains(&p) {
                continue;
            }
            if ext_of(&p) == "pdf" && stems.contains(&Path::new(&p).with_extension("").to_string_lossy().to_lowercase()) {
                continue;
            }
            let bytes = match std::fs::read(&p) {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("{}: {}", file_name(&p), e));
                    continue;
                }
            };
            // одинаковые файлы в разных папках («Ответы\ПР1_Ответы.docx» и «ПР1…\ПР1_Ответы.docx»)
            let h = (fnv(&bytes), bytes.len());
            if known_hashes.contains(&h) {
                dupes += 1;
                continue;
            }
            known_hashes.push(h);
            let ex = match extract_safe(&p, &bytes) {
                Ok(x) => x,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let name = file_name(&p);
            let meta = kinds.and_then(|k| k.get(&p)).cloned();
            let kind = meta.as_ref().map(|m| DocKind::from_str(&m.0)).unwrap_or_else(|| DocKind::guess_path(&p));
            let scope = meta
                .as_ref()
                .map(|m| m.1.clone())
                .unwrap_or_else(|| if scope.trim().is_empty() { program_in_path(&p, &programs).unwrap_or_default() } else { scope.trim().to_string() });
            let code = text::find_work_code(&name)
                .or_else(|| text::find_work_code(&parent_name(&p)))
                .or_else(|| {
                    let p2 = Path::new(&p).parent().and_then(|x| x.parent()).map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
                    text::find_work_code(&parent_name(&format!("{}/x", p2)))
                })
                .or_else(|| text::find_work_code(&ex.visible().lines().take(3).collect::<Vec<_>>().join(" ")))
                .unwrap_or_else(|| Path::new(&name).with_extension("").to_string_lossy().to_string());
            let code = meta.as_ref().map(|m| m.2.clone()).filter(|c| !c.is_empty()).unwrap_or(code);
            let id = self.new_id("t");
            fresh.push(TaskDoc { id, path: p.clone(), name, kind, code, scope, marked: ex.marked, notes: ex.notes, needs_pdf: ex.needs_pdf });
        }
        // один и тот же файл в двух папках в разных редакциях — оставляем более новый
        let mtime = |p: &str| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let mut keep: Vec<TaskDoc> = Vec::new();
        for t in fresh {
            match keep.iter().position(|k| k.name.to_lowercase() == t.name.to_lowercase() && k.code == t.code && k.scope == t.scope) {
                Some(i) => {
                    dupes += 1;
                    if mtime(&t.path) > mtime(&keep[i].path) {
                        keep[i] = t;
                    }
                }
                None => keep.push(t),
            }
        }
        let fresh = keep;
        if dupes > 0 {
            self.log(&format!("Пропущено повторяющихся файлов заданий: {}", dupes));
        }
        {
            let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            for t in fresh {
                if !ws.tasks.iter().any(|x| x.path == t.path) {
                    ws.tasks.push(t);
                }
            }
            self.rebuild_sets(&mut ws);
        }
        self.save_session();
        errors
    }

    pub fn rebuild_sets(&self, ws: &mut Workspace) {
        let markers = self.settings().get("custom_markers").str_list();
        ws.sets = assign::build_sets(&ws.tasks, &markers);
    }

    pub fn add_works(&self, paths: &[String], meta: Option<&HashMap<String, (String, Option<String>)>>, group: &str) -> Vec<String> {
        self.add_works_ex(paths, meta, group, meta.is_none())
    }

    /// `auto_pages`: фото рядом с единственным документом присоединяются к нему,
    /// фото одной работы («Лесовиков_1.jpg», «Лесовиков_2.jpg» или папка студента) — в одну работу.
    pub fn add_works_ex(&self, paths: &[String], meta: Option<&HashMap<String, (String, Option<String>)>>, group: &str, auto_pages: bool) -> Vec<String> {
        let files: Vec<String> = Core::expand(paths).into_iter().map(|x| x.0).collect();
        let mut errors = Vec::new();
        let known: Vec<String> = {
            let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            ws.works.iter().flat_map(|w| std::iter::once(w.path.clone()).chain(w.sources.iter().cloned())).collect()
        };
        let files: Vec<String> = files
            .into_iter()
            .filter(|p| !known.contains(p))
            .filter(|p| {
                // служебные файлы преподавателя (ведомости, ответы) — не работы студентов
                let lname = file_name(p).to_lowercase();
                !(lname.contains("ведомост") || lname.contains("_ответы") || lname.starts_with("ответы"))
            })
            .collect();
        let (mains, attach) = if auto_pages { group_pages(&files, paths) } else { (files, HashMap::new()) };
        let mut fresh: Vec<Work> = Vec::new();
        for p in mains {
            let bytes = match std::fs::read(&p) {
                Ok(b) => b,
                Err(e) => {
                    errors.push(format!("{}: {}", file_name(&p), e));
                    continue;
                }
            };
            let ex = match extract_safe(&p, &bytes) {
                Ok(x) => x,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            let (group, assignment) = match meta.and_then(|m| m.get(&p)) {
                Some((g, a)) => (g.clone(), a.clone()),
                None => (if group.trim().is_empty() { parent_name(&p) } else { group.trim().to_string() }, None),
            };
            let visible = ex.visible();
            let hidden = ex.hidden();
            let mut w = Work {
                id: self.new_id("w"),
                name: file_name(&p),
                group,
                path: p.clone(),
                visible,
                hidden,
                marked: ex.marked,
                notes: ex.notes,
                images: ex.images,
                needs_pdf: ex.needs_pdf,
                hash: fnv(&bytes),
                size: bytes.len() as u64,
                assignment,
                student: String::new(),
                pages: Vec::new(),
                sources: Vec::new(),
                transcribed: false,
                transcript: String::new(),
                urls: super::links::find_in_file(&p, &bytes, ""),
                link_docs: Vec::new(),
                pending_pdf: Vec::new(),
                extra_kinds: embedded_kinds(&bytes),
                xlsx: super::office::xlsx_facts(&bytes).map(|f| vec![(file_name(&p), f)]).unwrap_or_default(),
                deadline: 0,
                submitted: 0,
            };
            w.submitted = w.file_time(&self.data_dir);
            // файлы в папке студента Moodle, которые не читаются (схема draw.io, архив…), — тоже сданы
            for f in moodle_folder_files(&p) {
                let k = detect::file_kind(&f);
                if !SUPPORTED.contains(&ext_of(&f).as_str()) && k != "other" {
                    w.extra_kinds.push((k.to_string(), file_name(&f)));
                }
            }
            if ex.needs_images {
                w.pages.push(self.page_for(&p, &bytes));
            }
            // картинки внутри Word, PowerPoint, Excel: фото тетради, скриншоты, схемы —
            // их распознаёт модель с поддержкой изображений (решение — при проверке)
            if !ex.needs_images && !w.needs_pdf && bytes.starts_with(b"PK") {
                w.pages = self.media_pages(&w, &bytes);
            }
            if w.needs_pdf {
                // скан PDF, страницы которого уже были подготовлены раньше
                let pdf_pages = self.cached_pdf_pages(&w);
                if !pdf_pages.is_empty() {
                    w.pages = pdf_pages;
                    w.needs_pdf = false;
                    w.notes.push(format!("PDF без текста (скан): страниц — {}; текст распознаёт модель", w.pages.len()));
                }
            }
            // ответ из Moodle: ФИО — из имени файла или папки, имя файла — без служебной части
            let parent = Path::new(&w.path).parent().and_then(|x| x.file_name()).map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
            if let Some((s, _)) = super::moodle::parse_name(&w.name).or_else(|| super::moodle::parse_name(&parent)) {
                w.student = s;
            }
            if let Some(i) = w.name.find("_assignsubmission_") {
                let rest = &w.name[i + "_assignsubmission_".len()..];
                let short = rest.strip_prefix("file_").or_else(|| rest.strip_prefix("onlinetext_")).unwrap_or(rest).to_string();
                w.name = if short.starts_with("onlinetext") { "Текст ответа в Moodle".to_string() } else { short };
            }
            if let Some(extra) = attach.get(&p) {
                self.attach_sources(&mut w, extra);
            }
            self.load_cached_transcripts(&mut w);
            fresh.push(w);
        }
        {
            let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            for w in fresh {
                if ws.works.iter().any(|x| x.path == w.path) {
                    continue;
                }
                ws.outcomes.entry(w.id.clone()).or_insert_with(|| Outcome { status: "pending".into(), ..Default::default() });
                ws.works.push(w);
            }
            ws.works.sort_by(|a, b| a.group.cmp(&b.group).then(natural_key(&a.name).cmp(&natural_key(&b.name))));
        }
        self.save_session();
        errors
    }

    // -------------------------------------------------------------- фото, сканы, вложения

    fn pages_dir(&self) -> PathBuf {
        let d = self.data_dir.join("pages");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// Страница-изображение из файла; большое фото помечается для уменьшения в интерфейсе.
    pub fn page_for(&self, path: &str, bytes: &[u8]) -> Page {
        let hash = fnv(bytes);
        let cache = self.pages_dir().join(format!("{:016x}.jpg", hash));
        if cache.is_file() {
            return Page { src: path.to_string(), file: cache.to_string_lossy().to_string(), hash, size: bytes.len() as u64, ready: true };
        }
        let ext = ext_of(path);
        let ready = bytes.len() as u64 <= PAGE_MAX_BYTES && ["jpg", "jpeg", "png", "webp"].contains(&ext.as_str());
        Page { src: path.to_string(), file: path.to_string(), hash, size: bytes.len() as u64, ready }
    }

    /// Картинки, вставленные в документ (word/media, ppt/media, Pictures в .odt).
    fn media_pages(&self, w: &Work, bytes: &[u8]) -> Vec<Page> {
        let Ok(zip) = super::zip::ZipArchive::open(bytes) else { return Vec::new() };
        let mut names: Vec<String> = zip
            .names()
            .into_iter()
            .filter(|n| (n.starts_with("word/media/") || n.starts_with("ppt/media/") || n.starts_with("xl/media/") || n.starts_with("Pictures/")) && docx::is_image(n))
            .map(|n| n.to_string())
            .collect();
        names.sort_by_key(|n| natural_key(n));
        let dir = self.pages_dir();
        let mut out = Vec::new();
        for n in names.into_iter().take(60) {
            let Some(img) = zip.read(&n) else { continue };
            if img.len() < 8_000 {
                continue; // значки, маркеры списков
            }
            let hash = fnv(&img);
            let f = dir.join(format!("{:016x}-m.{}", hash, ext_of(&n)));
            if !f.is_file() && std::fs::write(&f, &img).is_err() {
                continue;
            }
            let mut pg = self.page_for(&f.to_string_lossy(), &img);
            pg.src = w.path.clone();
            out.push(pg);
        }
        out
    }

    fn cached_pdf_pages(&self, w: &Work) -> Vec<Page> {
        let dir = self.pages_dir();
        let mut out = Vec::new();
        for n in 1..=200 {
            let f = dir.join(format!("{:016x}-{}.jpg", w.hash, n));
            let Ok(b) = std::fs::read(&f) else { break };
            out.push(Page { src: w.path.clone(), file: f.to_string_lossy().to_string(), hash: fnv(&b), size: b.len() as u64, ready: true });
        }
        out
    }

    /// Страницы скана PDF, подготовленные интерфейсом (JPEG).
    pub fn set_pdf_pages(&self, w: &mut Work, images: &[Vec<u8>]) {
        let dir = self.pages_dir();
        let mut pages = Vec::new();
        for (i, b) in images.iter().enumerate() {
            let f = dir.join(format!("{:016x}-{}.jpg", w.hash, i + 1));
            if std::fs::write(&f, b).is_ok() {
                pages.push(Page { src: w.path.clone(), file: f.to_string_lossy().to_string(), hash: fnv(b), size: b.len() as u64, ready: true });
            }
        }
        let _ = std::fs::remove_file(dir.join(format!("{:016x}-{}.jpg", w.hash, images.len() + 1)));
        let src = w.path.clone();
        w.pages.retain(|p| p.src != src);
        let n = pages.len();
        pages.extend(w.pages.drain(..));
        w.pages = pages;
        w.needs_pdf = false;
        w.notes.retain(|x| !x.contains("PDF"));
        w.notes.push(format!("PDF без текста (скан): страниц — {}; текст распознаёт модель", n));
        self.reset_transcript(w);
        self.load_cached_transcripts(w);
    }

    /// Уменьшенная интерфейсом копия большого фото (или отказ: тогда шлём как есть).
    pub fn set_page_image(&self, w: &mut Work, idx: usize, jpeg: Option<&[u8]>) {
        let dir = self.pages_dir();
        if let Some(pg) = w.pages.get_mut(idx) {
            if let Some(b) = jpeg {
                let f = dir.join(format!("{:016x}.jpg", pg.hash));
                if std::fs::write(&f, b).is_ok() {
                    pg.file = f.to_string_lossy().to_string();
                }
            }
            pg.ready = true;
        }
    }

    /// Присоединяет к работе другие файлы: фото — как страницы, документы — текстом.
    pub fn attach_sources(&self, w: &mut Work, paths: &[String]) {
        for p in paths {
            if *p == w.path || w.sources.contains(p) {
                continue;
            }
            let bytes = match std::fs::read(p) {
                Ok(b) => b,
                Err(e) => {
                    w.notes.push(format!("Не удалось открыть «{}»: {}", file_name(p), e));
                    continue;
                }
            };
            w.sources.push(p.clone());
            w.size += bytes.len() as u64;
            for k in embedded_kinds(&bytes) {
                if !w.extra_kinds.contains(&k) {
                    w.extra_kinds.push(k);
                }
            }
            if let Some(f) = super::office::xlsx_facts(&bytes) {
                w.xlsx.push((file_name(p), f));
            }
            if docx::is_image(p) {
                w.pages.push(self.page_for(p, &bytes));
                w.images += 1;
                continue;
            }
            match extract_safe(p, &bytes) {
                Ok(ex) => {
                    if ex.needs_pdf {
                        // текст PDF прочитает интерфейс и пришлёт через set_source_text
                        w.pending_pdf.push(p.clone());
                        continue;
                    }
                    for u in super::links::find_in_file(p, &bytes, "") {
                        if !w.urls.contains(&u) {
                            w.urls.push(u);
                        }
                    }
                    let head = format!("\n\n=== Файл «{}» ===\n", file_name(p));
                    self.reset_transcript(w);
                    w.visible = format!("{}{}{}", w.visible, head, ex.visible()).trim().to_string();
                    w.marked = format!("{}{}{}", w.marked, head, ex.marked).trim().to_string();
                    w.hidden.extend(ex.hidden());
                    w.images += ex.images;
                    for n in ex.notes {
                        w.notes.push(format!("{}: {}", file_name(p), n));
                    }
                }
                Err(e) => w.notes.push(e),
            }
        }
        self.reset_transcript(w);
        self.load_cached_transcripts(w);
    }

    /// Текст присоединённого PDF (прочитан интерфейсом); `images` — страницы скана.
    pub fn set_source_pdf(&self, w: &mut Work, idx: usize, text: &str, images: &[Vec<u8>], failed: &str) {
        if idx >= w.pending_pdf.len() {
            return;
        }
        let p = w.pending_pdf.remove(idx);
        let name = file_name(&p);
        if !failed.is_empty() {
            w.notes.push(format!("PDF «{}» не прочитан: {}", name, failed));
            return;
        }
        self.reset_transcript(w);
        let t = super::text::tidy(text);
        if !t.trim().is_empty() {
            let head = format!("\n\n=== Файл «{}» ===\n", name);
            w.visible = format!("{}{}{}", w.visible, head, t).trim().to_string();
            w.marked = format!("{}{}{}", w.marked, head, t).trim().to_string();
        }
        if !images.is_empty() {
            let key = std::fs::read(&p).map(|b| fnv(&b)).unwrap_or(w.hash ^ 0x5bd1e995);
            let dir = self.pages_dir();
            for (i, b) in images.iter().enumerate() {
                let f = dir.join(format!("{:016x}-{}.jpg", key, i + 1));
                if std::fs::write(&f, b).is_ok() {
                    w.pages.push(Page { src: p.clone(), file: f.to_string_lossy().to_string(), hash: fnv(b), size: b.len() as u64, ready: true });
                }
            }
            w.notes.push(format!("PDF «{}» без текста (скан): страниц — {}; текст распознаёт модель", name, images.len()));
        }
        self.load_cached_transcripts(w);
    }

    pub fn transcript_cache(&self, hash: u64) -> PathBuf {
        self.pages_dir().join(format!("{:016x}.txt", hash))
    }

    fn load_cached_transcripts(&self, w: &mut Work) {
        if w.pages.is_empty() || w.transcribed {
            return;
        }
        let mut texts = Vec::new();
        for pg in &w.pages {
            match std::fs::read_to_string(self.transcript_cache(pg.hash)) {
                Ok(t) => texts.push(t),
                Err(_) => return,
            }
        }
        apply_transcript(w, &texts);
    }

    fn reset_transcript(&self, w: &mut Work) {
        if !w.transcript.is_empty() {
            if let Some(b) = w.visible.strip_suffix(&w.transcript) {
                w.visible = b.to_string();
            }
            if let Some(b) = w.marked.strip_suffix(&w.transcript) {
                w.marked = b.to_string();
            }
            w.transcript.clear();
        }
        w.transcribed = false;
    }

    /// Рубрика (критерии с баллами) набора заданий этой работы: settings.rubrics[set.id].
    pub fn rubric_for(&self, ws: &Workspace, w: &Work) -> Rubric {
        let Some(set) = self.set_for_work(ws, w) else { return Rubric::default() };
        Rubric::from_json(self.settings().get("rubrics").get(&set.id))
    }

    /// Набор заданий, к которому относится работа.
    /// Подходит ли набор заданий группе работы: «для всех», для этой группы или её направления.
    pub fn set_fits(&self, s: &AssignmentSet, group: &str, program: &str) -> bool {
        s.scope.is_empty() || s.scope == group || (!program.is_empty() && s.scope == program)
    }

    pub fn program_of(&self, group: &str) -> String {
        self.journal.lock().unwrap_or_else(|e| e.into_inner()).program_of(group)
    }

    /// Набор заданий, к которому относится работа.
    pub fn set_for_work<'a>(&self, ws: &'a Workspace, w: &Work) -> Option<&'a AssignmentSet> {
        if ws.sets.is_empty() {
            return None;
        }
        if let Some(id) = &w.assignment {
            if let Some(s) = ws.sets.iter().find(|s| &s.id == id).or_else(|| ws.sets.iter().find(|s| &s.code == id)) {
                return Some(s);
            }
        }
        let program = self.program_of(&w.group);
        let cands: Vec<&AssignmentSet> = {
            let fit: Vec<&AssignmentSet> = ws.sets.iter().filter(|s| self.set_fits(s, &w.group, &program)).collect();
            if fit.is_empty() {
                ws.sets.iter().collect()
            } else {
                fit
            }
        };
        // точное совпадение группы важнее направления, направление важнее «для всех»
        let rank = |s: &AssignmentSet| if s.scope == w.group { 0 } else if !s.scope.is_empty() { 1 } else { 2 };
        if let Some(code) = text::find_work_code(&w.name).or_else(|| text::find_work_code(&w.group)) {
            if let Some(s) = cands.iter().filter(|s| s.code == code).min_by_key(|s| rank(s)) {
                return Some(s);
            }
        }
        if cands.len() == 1 {
            return cands.first().copied();
        }
        // по содержимому
        let mut best: Option<&AssignmentSet> = None;
        let mut best_s = -1.0;
        for s in cands {
            let t: String = s.variants.values().map(|v| v.task_visible.as_str()).collect::<Vec<_>>().join(" ");
            let sc = text::word_overlap(&t, &w.visible);
            if sc > best_s {
                best_s = sc;
                best = Some(s);
            }
        }
        best
    }

    pub fn scan(&self, ws: &Workspace, w: &Work) -> Scan {
        let set = self.set_for_work(ws, w);
        let markers = self.settings().get("custom_markers").str_list();
        let mut sc = detect::scan_work(&w.visible, &w.hidden, &w.name, set, &markers);
        // вариант, назначенный студенту в списке группы
        if let Some((student, rv)) = self.roster_variant(w) {
            let declared = sc.variant_in_name.clone().or(sc.variant_in_text.clone());
            match declared {
                None => {
                    if set.map(|s| s.variants.contains_key(&rv) || s.variants.len() == 1).unwrap_or(true) {
                        sc.variant_used = Some(rv.clone());
                        sc.variant_source = "roster".into();
                    }
                }
                Some(d) if d != rv => sc.variant_notes.push(format!("По списку группы у студента {} вариант {}, а в работе указан вариант {}", student, rv, d)),
                _ => {}
            }
        }
        // какие файлы нужно было сдать и какие сданы
        let mut have: Vec<(String, String)> = std::iter::once(&w.path)
            .chain(w.sources.iter())
            .chain(w.pending_pdf.iter())
            .map(|p| (detect::file_kind(p).to_string(), file_name(p)))
            .collect();
        have.extend(w.extra_kinds.iter().cloned());
        // рисунок в документе (экспорт из draw.io, скриншот схемы) тоже считается схемой
        if w.images > 0 || w.pages.iter().any(|p| p.src == w.path) {
            have.push(("drawio".to_string(), "рисунок в документе".to_string()));
        }
        for u in w.all_urls() {
            let l = u.to_lowercase();
            let k = if l.contains("docs.google.com/spreadsheets") || l.contains("/spreadsheet") {
                "excel"
            } else if l.contains("docs.google.com/document") {
                "word"
            } else if l.contains("docs.google.com/presentation") {
                "powerpoint"
            } else if l.contains("diagrams.net") || l.contains("draw.io") {
                "drawio"
            } else {
                continue;
            };
            have.push((k.to_string(), "по ссылке".to_string()));
        }
        // в список файлов — только файлы и ссылки, без «фигур в документе» и т.п.
        sc.submitted_files = have
            .iter()
            .filter(|(_, n)| !n.ends_with("в документе") && n != "вставлен в документ")
            .map(|(k, n)| format!("{} — {}", n, if k == "drawio" { "схема draw.io" } else { detect::kind_label(k) }))
            .collect();
        if let Some(set) = set {
            // текст условия варианта (или общий для всех вариантов)
            let task_text: String = match sc.variant_used.as_ref().and_then(|v| set.variant(v)) {
                Some(v) => v.task_visible.clone(),
                None => set.variants.values().next().map(|v| v.task_visible.clone()).unwrap_or_default(),
            };
            self.excel_checks(w, &task_text, &mut sc);
            let groups = match sc.variant_used.as_ref().and_then(|v| set.variant(v)) {
                Some(v) => detect::required_files(&v.task_visible),
                None => {
                    // вариант не определён — только то, что требуют все варианты
                    let all: Vec<Vec<Vec<&'static str>>> = set.variants.values().map(|v| detect::required_files(&v.task_visible)).collect();
                    match all.first() {
                        Some(first) => first.iter().filter(|g| all.iter().all(|a| a.contains(g))).cloned().collect(),
                        None => Vec::new(),
                    }
                }
            };
            for g in groups {
                let label = g.iter().map(|k| detect::kind_label(k)).collect::<Vec<_>>().join(" или ");
                sc.required_files.push(label.clone());
                if !g.iter().any(|k| have.iter().any(|(h, _)| h == k)) {
                    sc.missing_files.push(label);
                }
            }
        }
        self.style_check(ws, w, &mut sc);
        sc
    }

    /// Проверка Excel без нейросети: формулы там, где условие требует расчётов,
    /// и диаграмма нужного вида.
    fn excel_checks(&self, w: &Work, task: &str, sc: &mut Scan) {
        let has_xlsx = !w.xlsx.is_empty();
        if detect::task_needs_formulas(task) && has_xlsx {
            for (file, f) in &w.xlsx {
                let pre = if w.xlsx.len() > 1 { format!("{}: ", file) } else { String::new() };
                if f.formulas == 0 && f.constants >= 4 {
                    sc.excel_findings.push(format!("{}в Excel нет ни одной формулы — все числа вбиты вручную", pre));
                    continue;
                }
                for m in &f.mixed {
                    sc.excel_findings.push(format!("{}значения без формул в {} (соседние ячейки столбца считаются формулами)", pre, m));
                }
                for t in &f.totals {
                    sc.excel_findings.push(format!("{}итог без формулы: {}", pre, t));
                }
                if f.formulas > 0 && f.mixed.is_empty() && f.totals.is_empty() {
                    sc.excel_ok.push(format!("{}расчёты сделаны формулами (формул: {})", pre, f.formulas));
                }
            }
        }
        if let Some(want) = detect::task_chart(task) {
            // диаграммы из Excel и вставленные в Word/PowerPoint
            let mut kinds: Vec<String> = w.xlsx.iter().flat_map(|(_, f)| f.charts.iter().cloned()).collect();
            for line in w.visible.lines() {
                if let Some(rest) = line.trim_start_matches('[').strip_prefix("Диаграмма: ") {
                    kinds.extend(rest.split([',', ';']).next().unwrap_or("").split(" + ").map(|x| x.trim().to_string()).filter(|x| !x.is_empty() && x != "тип не определён"));
                }
            }
            // диаграмма, вставленная картинкой (описала модель с поддержкой изображений)
            let pic = w.transcript.to_lowercase().contains("диаграмм");
            kinds.sort();
            kinds.dedup();
            let has_any = !kinds.is_empty() || w.visible.contains("Диаграмма: ") || pic;
            if !has_any {
                if has_xlsx || w.visible.trim().len() > 0 {
                    sc.excel_findings.push("по условию нужна диаграмма, а в работе её нет".into());
                }
            } else if !want.is_empty() {
                let ok = want.iter().any(|root| kinds.iter().any(|k| k.contains(root)));
                if !ok {
                    let names: Vec<&str> = want
                        .iter()
                        .map(|r| match *r {
                            "кругов" => "круговая",
                            "гистограмм" => "гистограмма",
                            "точечн" => "точечная",
                            "кольцев" => "кольцевая",
                            "лепестк" => "лепестковая",
                            "график" => "график",
                            x => x,
                        })
                        .collect();
                    sc.excel_findings.push(format!("нужна диаграмма вида «{}», а построена: {}", names.join(" / "), if kinds.is_empty() { "вид не определён".to_string() } else { kinds.join(", ") }));
                } else {
                    sc.excel_ok.push(format!("диаграмма нужного вида есть ({})", kinds.join(", ")));
                }
            } else {
                sc.excel_ok.push(format!("диаграмма есть ({})", if kinds.is_empty() { "вид не определён".to_string() } else { kinds.join(", ") }));
            }
        }
    }

    pub fn compute_plagiarism(&self) {
        let threshold = self.settings().get("penalties").get("plagiarism_threshold").f64_or(0.6);
        let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        // описания картинок внутри документов не сравниваем: у всех студентов
        // одни и те же скриншоты, а модель описывает их похожими словами
        let texts: Vec<String> = ws
            .works
            .iter()
            .map(|w| if w.transcribed && w.embedded_pictures() && w.visible.split_whitespace().count() > 80 { w.visible.strip_suffix(&w.transcript).unwrap_or(&w.visible).to_string() } else { w.visible.clone() })
            .collect();
        let tasks: Vec<String> = ws.sets.iter().flat_map(|s| s.variants.values().map(|v| format!("{}\n{}", v.task_visible, v.key))).collect();
        let mut pairs: Vec<(String, String, f64)> = detect::plagiarism(&texts, &tasks, threshold)
            .into_iter()
            .map(|p| (ws.works[p.a].id.clone(), ws.works[p.b].id.clone(), p.score))
            .collect();
        // одинаковые файлы
        for i in 0..ws.works.len() {
            for j in i + 1..ws.works.len() {
                if ws.works[i].hash == ws.works[j].hash && ws.works[i].size > 0 {
                    let (a, b) = (ws.works[i].id.clone(), ws.works[j].id.clone());
                    if !pairs.iter().any(|p| p.0 == a && p.1 == b) {
                        pairs.push((a, b, 1.0));
                    }
                }
            }
        }
        // одна и та же работа пары, сданная обоими напарниками, — не списывание
        let authors: HashMap<String, Vec<String>> = ws.works.iter().map(|w| (w.id.clone(), self.authors(&ws, w).0)).collect();
        let mut team_dups: Vec<(String, String)> = Vec::new();
        pairs.retain(|(a, b, _)| {
            let (x, y) = (authors.get(a).cloned().unwrap_or_default(), authors.get(b).cloned().unwrap_or_default());
            let same_team = (x.len() > 1 || y.len() > 1) && x.iter().any(|n| y.contains(n));
            if same_team {
                team_dups.push((a.clone(), b.clone()));
            }
            !same_team
        });
        ws.plag = pairs;
        ws.team_dups = team_dups;
        // архив прошлых лет: та же работа (шифр) у студентов прежних групп
        let arch = self.settings().get("archive").clone();
        ws.archive_hits = if arch.get("enabled").bool_or(true) {
            let task_sh: HashSet<u64> = tasks.iter().flat_map(|t| detect::shingle_set(t)).collect();
            let items: Vec<(String, String, String, HashSet<u64>)> = ws
                .works
                .iter()
                .zip(texts.iter())
                .map(|(w, t)| {
                    let code = self.set_for_work(&ws, w).map(|s| s.code.clone()).unwrap_or_default();
                    let sh: HashSet<u64> = detect::shingle_set(t).difference(&task_sh).copied().collect();
                    (w.id.clone(), code, format!("{:x}", w.hash), sh)
                })
                .collect();
            super::history::arc_matches(&self.data_dir, &items, threshold, arch.get("all_codes").bool_or(false))
        } else {
            HashMap::new()
        };
        drop(ws);
        self.refinalize_plag();
    }

    /// Пересчитывает итог проверенных работ, у которых есть совпадения или
    /// «2» за списывание: состав одинаковых и правило могли измениться.
    /// Пересчитать итог всех проверенных работ (после смены правил).
    pub fn refinalize_all(&self) {
        let ids: Vec<String> = {
            let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            ws.outcomes.iter().filter(|(_, o)| o.status == "done" && !o.llm.is_null()).map(|(id, _)| id.clone()).collect()
        };
        for id in ids {
            let _ = self.refinalize(&id);
        }
    }

    pub fn refinalize_plag(&self) {
        let ids: Vec<String> = {
            let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            ws.outcomes
                .iter()
                .filter(|(id, o)| o.status == "done" && !o.llm.is_null() && (o.violation == "plagiarism" || !o.plag.is_empty() || ws.plag.iter().any(|p| &p.0 == *id || &p.1 == *id)))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let _ = self.refinalize(&id);
        }
    }

    /// Совпадения работы с другими и решение, наказывать ли за них:
    /// в настройках задано, сколько одинаковых работ можно зачесть.
    pub fn plag_info(&self, ws: &Workspace, id: &str) -> PlagInfo {
        let list = self.plag_for(ws, id);
        if list.is_empty() {
            return PlagInfo::default();
        }
        let st = self.settings();
        let pen = st.get("penalties");
        let allowed = pen.get("plagiarism_allowed").i64_or(1).clamp(1, 50) as usize;
        let keep_first = pen.get("plagiarism_over").str_or("all") == "first";
        // группа одинаковых: все работы, связанные совпадениями (A≈B, B≈C → A, B, C)
        let mut cluster: Vec<String> = vec![id.to_string()];
        let mut i = 0;
        while i < cluster.len() {
            let cur = cluster[i].clone();
            for (a, b, _) in &ws.plag {
                let other = if *a == cur { b } else if *b == cur { a } else { continue };
                if !cluster.contains(other) {
                    cluster.push(other.clone());
                }
            }
            i += 1;
        }
        // кто сдал раньше: по времени файла (у Moodle — время из архива), затем по имени
        let mut order: Vec<(u64, Vec<(u8, u64, String)>, String, String)> = cluster
            .iter()
            .filter_map(|cid| ws.works.iter().find(|w| &w.id == cid))
            .map(|w| {
                let t = match w.submitted_at(&self.data_dir) {
                    0 => u64::MAX,
                    t => t,
                };
                let who = if w.student.is_empty() { w.name.clone() } else { w.student.clone() };
                (t, natural_key(&w.name), w.id.clone(), who)
            })
            .collect();
        order.sort();
        let n = order.len();
        let pos = order.iter().position(|x| x.2 == id).unwrap_or(0);
        let first: Vec<String> = order.iter().take(allowed).map(|x| x.3.clone()).collect();
        let (penalize, note) = if n <= allowed {
            (false, format!("одинаковых работ {} — можно зачесть до {}, работа зачтена", n, allowed))
        } else if keep_first && pos < allowed {
            (false, format!("одинаковых работ {}, зачтены первые сданные ({}) — эта среди них", n, allowed))
        } else if keep_first {
            (true, format!("одинаковых работ {}, зачтены только первые сданные: {}", n, first.join(", ")))
        } else if allowed > 1 {
            (true, format!("одинаковых работ {} — больше, чем можно зачесть ({})", n, allowed))
        } else {
            (true, String::new())
        };
        PlagInfo { list, penalize, note, size: n }
    }

    pub fn plag_for(&self, ws: &Workspace, id: &str) -> Vec<(String, f64)> {
        ws.plag
            .iter()
            .filter_map(|(a, b, s)| {
                let other = if a == id { b } else if b == id { a } else { return None };
                let w = ws.works.iter().find(|w| &w.id == other)?;
                Some((format!("{}{}", if w.group.is_empty() { String::new() } else { format!("{}/", w.group) }, w.name), *s))
            })
            .collect()
    }

    // -------------------------------------------------------------- итоговая оценка

    /// Доводы модели об ИИ, которые действительно есть в тексте работы.
    /// Возвращает (подтверждённые [(довод, фрагмент)], неподтверждённые).
    pub fn verify_model_evidence(w: &Work, llm: &Json) -> (Vec<(String, String)>, Vec<String>) {
        let hay = text::norm(&w.visible);
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        for item in llm.get("ai_evidence").str_list() {
            let item = item.trim().to_string();
            if item.is_empty() {
                continue;
            }
            // в доводе могут быть цитаты «…» или "…" — проверяем их, иначе весь довод
            let mut quotes: Vec<String> = Vec::new();
            for (open, close) in [('«', '»'), ('"', '"'), ('“', '”')] {
                let mut rest = item.as_str();
                while let Some(a) = rest.find(open) {
                    let after = &rest[a + open.len_utf8()..];
                    let Some(b) = after.find(close) else { break };
                    let q = after[..b].trim();
                    if q.chars().count() >= 3 {
                        quotes.push(q.to_string());
                    }
                    rest = &after[b + close.len_utf8()..];
                }
            }
            if quotes.is_empty() {
                quotes.push(item.clone());
            }
            match quotes.iter().find(|q| text::contains_phrase(&hay, q)) {
                Some(q) => {
                    let snip = text::snippet_around(&w.visible, q, 90).unwrap_or_default();
                    ok.push((item.clone(), snip));
                }
                None => bad.push(item),
            }
        }
        (ok, bad)
    }

    /// Применяет правила преподавателя к ответу модели.
    /// `decision` — решение преподавателя по подозрению на ИИ («», keep, two).
    pub fn finalize(&self, w: &Work, scan: &Scan, llm: &Json, pinfo: &PlagInfo, decision: &str) -> Verdict {
        let plag = &pinfo.list;
        let st = self.settings();
        let pen = st.get("penalties");
        let half = st.get("half_grades").bool_or(true);
        let mut grade = norm_grade(llm.get("grade").f64_or(2.0), half);
        let mut reasons: Vec<String> = Vec::new();
        let mut violation = String::new();
        let mut review: Vec<String> = Vec::new();
        let g = |x: f64| fmt_grade(x);

        // ограничения «максимум»
        let cap = |max: f64, why: String, grade: &mut f64, reasons: &mut Vec<String>| {
            if *grade > max {
                *grade = max;
            }
            if !reasons.contains(&why) {
                reasons.push(why);
            }
        };
        let lname = format!("{} {}", w.name, w.group).to_lowercase().replace('ё', "е");
        for c in st.get("caps").as_arr() {
            if !c.get("enabled").bool_or(true) {
                continue;
            }
            let hit = c.get("pattern").str_or("").split('|').map(|x| x.trim().to_lowercase().replace('ё', "е")).filter(|x| !x.is_empty()).any(|x| lname.contains(&x));
            if hit {
                let label = c.get("label").str_or("Ограничение по правилу");
                if c.get("mode").str_or("max") == "minus" {
                    // «минус N баллов» (например, за опоздание), но не ниже 2
                    let n = c.get("max").f64_or(1.0).clamp(0.5, 3.0);
                    grade = (grade - n).max(2.0);
                    if !reasons.contains(&label) {
                        reasons.push(label);
                    }
                } else {
                    cap(c.get("max").f64_or(3.0), label, &mut grade, &mut reasons);
                }
            }
        }
        let rules = st.get("rules").clone();
        let mw = rules.get("min_words");
        if mw.get("enabled").bool_or(false) && scan.word_count < mw.get("words").i64_or(150).max(1) as usize {
            let m = mw.get("max").f64_or(3.0);
            cap(m, format!("Слишком мало текста ({} слов) — максимум «{}»", scan.word_count, g(m)), &mut grade, &mut reasons);
        }
        let ss = rules.get("screenshots");
        if ss.get("enabled").bool_or(false) && scan.task_needs_images && w.images == 0 {
            let m = ss.get("max").f64_or(4.0);
            cap(m, format!("В условии нужны скриншоты, а в работе нет ни одного изображения — максимум «{}»", g(m)), &mut grade, &mut reasons);
        }
        let xr = rules.get("excel_checks");
        if !scan.excel_findings.is_empty() {
            let list = scan.excel_findings.join("; ");
            if xr.get("enabled").bool_or(true) {
                let m = xr.get("max").f64_or(4.0);
                cap(m, format!("Проверка Excel: {} — максимум «{}»", list, g(m)), &mut grade, &mut reasons);
            } else {
                reasons.push(format!("Проверка Excel: {}", list));
            }
        }
        let lr = rules.get("late");
        if lr.get("enabled").bool_or(true) {
            if let Some((at, dl)) = w.late(&self.data_dir) {
                let m = lr.get("max").f64_or(3.0);
                let hours = (at - dl) as f64 / 3600.0;
                let by = if hours < 24.0 { format!("на {} ч", hours.ceil() as i64) } else { format!("на {} дн.", (hours / 24.0).ceil() as i64) };
                cap(m, format!("Сдано позже срока {}: {} (срок {}) — максимум «{}»", by, super::report::fmt_dt(at), super::report::fmt_dt(dl), g(m)), &mut grade, &mut reasons);
            }
        }
        let mf = rules.get("missing_files");
        if mf.get("enabled").bool_or(true) && !scan.missing_files.is_empty() {
            let m = mf.get("max").f64_or(3.0);
            cap(m, format!("Не сдан обязательный файл по условию: {} — максимум «{}»", scan.missing_files.join(", "), g(m)), &mut grade, &mut reasons);
        }
        let vcap = st.get("variant_missing_cap").f64_or(3.0);
        if vcap > 0.0 && scan.variant_in_text.is_none() && scan.variant_in_name.is_none() {
            cap(vcap, format!("Номер варианта не указан ни в названии файла, ни в тексте работы — максимум «{}»", g(vcap)), &mut grade, &mut reasons);
        }
        for n in &scan.variant_notes {
            reasons.push(n.clone());
        }
        if scan.variant_source == "roster" {
            reasons.push(format!("Номер варианта в работе не указан — проверено по варианту {} из списка группы", scan.variant_used.clone().unwrap_or_default()));
        }
        if st.get("completeness_rules").bool_or(true) {
            let c = llm.get("completeness").i64_or(100);
            if c < 34 {
                cap(2.0, format!("Выполнено менее трети работы (~{} %)", c), &mut grade, &mut reasons);
            } else if c < 60 {
                cap(3.0, format!("Выполнено около половины работы (~{} %) — максимум «3»", c), &mut grade, &mut reasons);
            }
        }
        let mut wrong_variant: Option<String> = None;
        if let (Some(b), Some(d)) = (&scan.best_match_variant, scan.variant_in_name.as_ref().or(scan.variant_in_text.as_ref())) {
            if b != d && scan.best_match_score - scan.declared_match_score > 0.25 && scan.declared_match_score < 0.3 {
                let wv = rules.get("wrong_variant");
                let why = format!("По содержанию работа похожа на вариант {}, а указан вариант {}", b, d);
                wrong_variant = Some(why.clone());
                if wv.get("enabled").bool_or(false) {
                    let m = wv.get("max").f64_or(2.0);
                    cap(m, format!("{} — выполнен чужой вариант, максимум «{}»", why, g(m)), &mut grade, &mut reasons);
                } else {
                    reasons.push(why);
                }
            }
        }
        let _ = wrong_variant;
        let content_grade = grade;

        // ---- признаки ИИ ----------------------------------------------------
        let conf = {
            let c = llm.get("ai_confidence").f64_or(0.0);
            if c > 1.0 { c / 100.0 } else { c }
        };
        let thr = pen.get("ai_confidence").f64_or(0.7);
        let (model_ok, model_bad) = Core::verify_model_evidence(w, llm);
        let scan_signs: Vec<&super::detect::Evidence> = scan.evidence.iter().filter(|e| (e.weight == "strong" || e.weight == "medium") && e.kind != "injection").collect();
        // вывод модели засчитывается, только если её доводы есть в тексте
        // или их подтверждает автоматическая проверка: маленькие модели любят
        // «находить» в работе слова-ловушки, которых там нет
        let model_says = llm.get("ai_detected").bool_or(false);
        let model_trusted = model_says && (!model_ok.is_empty() || !scan_signs.is_empty());
        if model_says && !model_trusted {
            review.push(format!(
                "Нейросеть заподозрила ИИ ({} %), но её доводы в тексте работы не найдены{} — вывод не учтён",
                (conf * 100.0).round(),
                if model_bad.is_empty() { String::new() } else { format!(": {}", model_bad.join("; ")) }
            ));
        }
        // все найденные признаки с местом в тексте — для окна решения
        let mut signs = Json::arr();
        for e in &scan_signs {
            signs.push(Json::obj().with("what", e.what.as_str()).with("snippet", e.snippet.as_str()).with("weight", e.weight.as_str()).with("source", "автопроверка"));
        }
        if model_trusted {
            for (what, snip) in &model_ok {
                signs.push(Json::obj().with("what", what.as_str()).with("snippet", snip.as_str()).with("weight", "medium").with("source", "нейросеть"));
            }
        }
        let inj: Vec<&super::detect::Evidence> = scan.evidence.iter().filter(|e| e.kind == "injection" && e.weight == "strong").collect();
        for e in &inj {
            signs.push(Json::obj().with("what", e.what.as_str()).with("snippet", e.snippet.as_str()).with("weight", "strong").with("source", "приписка для нейросети"));
        }

        let mut ai_reasons: Vec<String> = Vec::new();
        if pen.get("enabled").bool_or(true) {
            let strong: Vec<String> = scan_signs.iter().filter(|e| e.weight == "strong").map(|e| e.what.clone()).collect();
            if pen.get("ai_markers").bool_or(true) && !strong.is_empty() {
                ai_reasons.push(format!("сработали ловушки: {}", strong.join("; ")));
            }
            if pen.get("ai_model").bool_or(true) && model_trusted && conf >= thr {
                let ev: Vec<String> = model_ok.iter().map(|(w, _)| w.clone()).collect();
                ai_reasons.push(format!("вывод нейросети (уверенность {} %){}", (conf * 100.0).round(), if ev.is_empty() { String::new() } else { format!(": {}", ev.join("; ")) }));
            }
            if pen.get("ai_multi").bool_or(true) {
                let mut n: Vec<String> = scan_signs.iter().map(|e| e.what.clone()).collect();
                if model_trusted {
                    n.push(format!("вывод нейросети ({} %)", (conf * 100.0).round()));
                }
                let need = pen.get("ai_multi_count").i64_or(2).max(2) as usize;
                if n.len() >= need && ai_reasons.is_empty() {
                    ai_reasons.push(format!("найдено признаков: {} (порог {}): {}", n.len(), need, n.join("; ")));
                }
            }
            if pen.get("injection").bool_or(true) && !inj.is_empty() {
                ai_reasons.push(format!("попытка повлиять на проверку (ИИ-инъекция): {}", inj.iter().map(|e| e.what.clone()).collect::<Vec<_>>().join("; ")));
            }
        } else if !scan_signs.is_empty() || model_trusted {
            reasons.push("Есть признаки ИИ (автоматическая «2» отключена в настройках)".into());
        }

        let mut suspect = Json::Null;
        if !ai_reasons.is_empty() {
            let ask = pen.get("decide").str_or("ask") == "ask";
            let why = format!("Использование ИИ — {}", ai_reasons.join("; "));
            match (ask, decision) {
                (_, "keep") => {
                    reasons.push(format!("Признаки ИИ были ({}), но преподаватель решил оценить по содержанию", ai_reasons.join("; ")));
                }
                (false, _) | (_, "two") => {
                    grade = 2.0;
                    violation = "ai".into();
                    reasons.insert(0, why);
                }
                (true, _) => {
                    // ждём решения преподавателя: оценка пока по содержанию
                    suspect = Json::obj()
                        .with("content_grade", content_grade)
                        .with("content_grade_text", fmt_grade(content_grade))
                        .with("reasons", ai_reasons.clone())
                        .with("signs", signs.clone())
                        .with("model_confidence", (conf * 100.0).round())
                        .with("model_unverified", model_bad.clone());
                    review.insert(0, "Есть признаки ИИ — нужно ваше решение: оценка по содержанию или «2»".into());
                }
            }
        }

        if pen.get("enabled").bool_or(true) {
            if pen.get("plagiarism").bool_or(true) && !plag.is_empty() {
                let list = plag.iter().map(|(n, s)| format!("{} ({} %)", n, (s * 100.0).round())).collect::<Vec<_>>().join(", ");
                let note = if pinfo.note.is_empty() { String::new() } else { format!("; {}", pinfo.note) };
                if pinfo.penalize {
                    grade = 2.0;
                    if violation.is_empty() {
                        violation = "plagiarism".into();
                    }
                    reasons.insert(0, format!("Списывание — совпадение текста с работой: {}{}", list, note));
                } else {
                    reasons.push(format!("Совпадает с работой: {}{}", list, note));
                }
            }
        } else if !plag.is_empty() {
            reasons.push("Есть совпадения с другими работами (автоматическая «2» отключена)".into());
        }
        Verdict { grade: norm_grade(grade, half), reasons, violation, suspect, review }
    }

    /// Пересчитывает итог по уже сохранённому ответу модели (после решения преподавателя).
    // -------------------------------------------------------------- кэш результатов

    /// Отпечаток всего, от чего зависит ответ нейросети: файлы работы, условие и ключ
    /// варианта, критерии, режим, модели. Совпал — работу можно не проверять заново.
    /// Раздел запроса «как преподаватель исправлял оценки за это задание» (пусто — нет примеров
    /// или учёт выключен).
    pub fn teacher_examples(&self, id: &str) -> String {
        let st = self.settings();
        if !st.get("learn").get("enabled").bool_or(true) {
            return String::new();
        }
        let _ = st;
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        match ws.works.iter().find(|w| w.id == id) {
            Some(w) => self.teacher_examples_ws(&ws, w),
            None => String::new(),
        }
    }

    pub fn teacher_examples_ws(&self, ws: &Workspace, w: &Work) -> String {
        let st = self.settings();
        if !st.get("learn").get("enabled").bool_or(true) {
            return String::new();
        }
        let n = st.get("learn").get("examples").i64_or(3).clamp(0, 6) as usize;
        let Some(set) = self.set_for_work(ws, w) else { return String::new() };
        let sc = self.scan(ws, w);
        let variant = sc.variant_used.or(sc.variant_in_name).or(sc.variant_in_text).unwrap_or_default();
        let list = super::lessons::lessons_for(&self.data_dir, &set.code, &variant, &format!("{:x}", w.hash), &w.visible, n);
        super::lessons::lessons_prompt(&list, fmt_grade)
    }

    /// Правка оценки преподавателем → урок для следующих проверок этого задания
    /// (оценка отличается от поставленной проверкой) и замечания из комментария — в банк.
    pub fn learn_from_override(&self, id: &str, before_comment: &str) {
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let Some(w) = ws.works.iter().find(|w| w.id == id) else { return };
        let Some(o) = ws.outcomes.get(id) else { return };
        let key = format!("{:x}", w.hash);
        let code = self.set_for_work(&ws, w).map(|s| s.code.clone()).unwrap_or_default();
        let names = format!("{} {}", w.student, self.authors(&ws, w).0.join(" "));
        super::lessons::remarks_learn(&self.data_dir, &code, &o.override_comment, before_comment, &names);
        let model = o.grade;
        match (o.override_grade, model) {
            (Some(t), Some(m)) if (t - m).abs() > 0.01 && !code.is_empty() && !o.llm.is_null() => {
                let sc = self.scan(&ws, w);
                let variant = sc.variant_used.or(sc.variant_in_name).or(sc.variant_in_text).unwrap_or_default();
                let mut said = o.llm.get("summary").str_or("");
                let errs: Vec<String> = o.llm.get("errors").as_arr().iter().take(3).map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_else(|| e.get("what").str_or(&e.get("text").str_or("")))).filter(|s| !s.trim().is_empty()).collect();
                if !errs.is_empty() {
                    said.push_str(&format!(" Ошибки: {}", errs.join("; ")));
                }
                // ФИО студента в пример не попадает
                for part in names.split_whitespace().filter(|x| x.chars().count() >= 3) {
                    said = said.replace(part, "…");
                }
                let mut why = o.override_comment.clone();
                for part in names.split_whitespace().filter(|x| x.chars().count() >= 3) {
                    why = why.replace(part, "…");
                }
                super::lessons::lesson_put(
                    &self.data_dir,
                    super::lessons::Lesson { key, code, variant, model_grade: m, teacher_grade: t, why, model_said: super::text::clip(said.trim(), 600), sketch: super::lessons::sketch(&w.visible), date: now_s() },
                );
            }
            _ => super::lessons::lesson_forget(&self.data_dir, &key),
        }
    }

    pub fn fingerprint(&self, ws: &Workspace, w: &Work) -> String {
        let mut parts: Vec<String> = vec!["cache-v1".into(), format!("{:x}", w.hash)];
        for src in &w.sources {
            parts.push(std::fs::read(src).map(|b| format!("{:x}", fnv(&b))).unwrap_or_else(|_| src.clone()));
        }
        if let Some(set) = self.set_for_work(ws, w) {
            parts.push(set.id.clone());
            let sc = self.scan(ws, w);
            let vtext = |v: &super::assign::Variant| format!("{}\n{}\n{}\n{}", v.task_visible, v.key, v.hidden.join("|"), v.ai_row);
            match sc.variant_used.as_ref().and_then(|v| set.variant(v)) {
                Some(v) => parts.push(format!("{:x}", fnv(vtext(v).as_bytes()))),
                None => parts.push(format!("{:x}", fnv(set.variants.values().map(vtext).collect::<Vec<_>>().join("\n").as_bytes()))),
            }
            parts.push(format!("{:x}", fnv(format!("{}\n{}", set.general_key, set.example).as_bytes())));
            // критерии рубрики (пороги оценок — нет: оценка по ним пересчитывается без новой проверки)
            let items = self.settings().get("rubrics").get(&set.id).get("items").clone();
            if !items.as_arr().is_empty() {
                parts.push(items.to_string());
            }
        }
        let st = self.settings();
        parts.push(st.get("criteria").to_string());
        parts.push(st.get("extra_instructions").to_string());
        parts.push(st.get("mode").str_or("direct"));
        parts.push(st.get("double_check").to_string());
        parts.push(st.get("links").get("enabled").to_string());
        // новые параметры — только если отличаются от прежнего поведения (старый кэш остаётся годным)
        let style = st.get("answer_style").str_or("full");
        if style != "full" {
            parts.push(format!("answer:{}", style));
        }
        let ex = self.teacher_examples_ws(ws, w);
        if !ex.is_empty() {
            parts.push(format!("examples:{:x}", fnv(ex.as_bytes())));
        }
        if let Some(p) = super::grader::active_provider(self) {
            parts.push(format!("{}|{}|{}", p.kind, p.base_url, p.model));
        }
        format!("{:016x}", fnv(parts.join("\u{1}").as_bytes()))
    }

    fn results_dir(&self) -> PathBuf {
        let d = self.data_dir.join("results");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// Запомнить результат проверки по отпечатку.
    pub fn cache_store(&self, wid: &str) {
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let Some(w) = ws.works.iter().find(|w| w.id == wid) else { return };
        let Some(o) = ws.outcomes.get(wid) else { return };
        if o.status != "done" || o.llm.is_null() || o.fingerprint.is_empty() {
            return;
        }
        let j = o.to_json(&w.id, &w.path);
        let f = self.results_dir().join(format!("{}.json", o.fingerprint));
        drop(ws);
        let _ = std::fs::write(f, j.to_string());
    }

    /// Взять результат из кэша, если работа, задание и модель не менялись.
    /// Правила оценки применяются заново (они могли измениться).
    pub fn cache_restore(&self, wid: &str) -> bool {
        if !self.settings().get("cache_results").bool_or(true) {
            return false;
        }
        let fp = {
            let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            match ws.works.iter().find(|w| w.id == wid) {
                Some(w) => self.fingerprint(&ws, w),
                None => return false,
            }
        };
        let Some(saved) = std::fs::read_to_string(self.results_dir().join(format!("{}.json", fp))).ok().and_then(|t| json::parse(&t).ok()) else { return false };
        {
            let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            let prev = ws.outcomes.get(wid).cloned().unwrap_or_default();
            let mut o = Outcome::from_json(&saved);
            if o.llm.is_null() {
                return false;
            }
            o.status = "done".into();
            o.fingerprint = fp;
            if o.from_cache == 0 {
                o.from_cache = o.checked_at.max(1);
            }
            // свои пометки преподавателя в текущей сессии важнее сохранённых
            if prev.override_grade.is_some() || !prev.override_violation.is_empty() || !prev.override_comment.is_empty() {
                o.override_grade = prev.override_grade;
                o.override_violation = prev.override_violation.clone();
                o.override_comment = prev.override_comment.clone();
            }
            if !prev.feedback.is_empty() {
                o.feedback = prev.feedback.clone();
            }
            if !prev.oral.is_null() {
                o.oral = prev.oral.clone();
            }
            o.tokens_in = 0;
            o.tokens_out = 0;
            ws.outcomes.insert(wid.to_string(), o);
        }
        let _ = self.refinalize(wid);
        true
    }

    pub fn refinalize(&self, wid: &str) -> Result<(), String> {
        let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let mut w = ws.works.iter().find(|w| w.id == wid).cloned().ok_or("работа не найдена")?;
        w.deadline = self.deadline_of(&ws, &w);
        let scan = self.scan(&ws, &w);
        let pinfo = self.plag_info(&ws, wid);
        if ws.outcomes.get(wid).map(|o| o.llm.is_null()).unwrap_or(true) {
            return Err("работа ещё не проверена".into());
        }
        let rub = {
            let set = self.set_for_work(&ws, &w).map(|s| s.id.clone()).unwrap_or_default();
            Rubric::from_json(self.settings().get("rubrics").get(&set))
        };
        let o = ws.outcomes.get_mut(wid).ok_or("работа ещё не проверена")?;
        let mut llm = o.llm.clone();
        if let Some(m) = o.llm.get("merged_grade").as_f64() {
            llm.set("grade", m);
        }
        // рубрика: оценка — по сумме баллов (рубрику можно поменять и пересчитать без новой проверки)
        let half = self.settings().get("half_grades").bool_or(true);
        if rub.apply(&mut llm, half) {
            o.llm.set("rubric", llm.get("rubric").clone());
            o.llm.set("rubric_total", llm.get("rubric_total").clone());
            o.llm.set("rubric_max", llm.get("rubric_max").clone());
            o.llm.set("rubric_grade", llm.get("rubric_grade").clone());
        } else if !o.llm.get("rubric").is_null() {
            // рубрику убрали — снова оценка модели
            if let Some(mg) = o.llm.get("model_grade").as_f64() {
                if o.llm.get("merged_grade").is_null() {
                    llm.set("grade", mg);
                }
            }
            o.llm.set("rubric", Json::Null);
            o.llm.set("rubric_grade", Json::Null);
        }
        let v = self.finalize(&w, &scan, &llm, &pinfo, &o.ai_decision);
        o.plag = pinfo.list;
        o.grade = Some(v.grade);
        o.reasons = v.reasons;
        o.set_auto_violation(v.violation);
        o.ai_suspect = v.suspect;
        o.review.retain(|r| !r.starts_with("Есть признаки ИИ — нужно ваше решение"));
        for r in v.review {
            if !o.review.contains(&r) {
                o.review.insert(0, r);
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- сессия

    pub fn save_settings(&self) {
        let s = self.settings().pretty();
        let _ = std::fs::write(self.data_dir.join("settings.json"), s);
    }

    pub fn save_session(&self) {
        if self.restoring.load(Ordering::SeqCst) {
            return;
        }
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks = Json::arr();
        for t in &ws.tasks {
            tasks.push(Json::obj().with("path", t.path.as_str()).with("kind", t.kind.as_str()).with("scope", t.scope.as_str()).with("code", t.code.as_str()));
        }
        let mut works = Json::arr();
        for w in &ws.works {
            let mut o = Json::obj().with("path", w.path.as_str()).with("group", w.group.as_str()).with("assignment", w.assignment.clone());
            if !w.student.is_empty() {
                o.set("student", w.student.as_str());
            }
            if !w.sources.is_empty() {
                o.set("sources", w.sources.clone());
            }
            if w.deadline > 0 {
                o.set("deadline", w.deadline);
            }
            if w.submitted > 0 {
                o.set("submitted", w.submitted);
            }
            if ws.paused.contains(&w.id) {
                o.set("paused", true);
            }
            if let Some(oc) = ws.outcomes.get(&w.id) {
                if oc.status == "done" || oc.override_grade.is_some() {
                    o.set("outcome", oc.to_json(&w.id, &w.path));
                }
            }
            works.push(o);
        }
        let mut doc = Json::obj().with("version", 1).with("tasks", tasks).with("works", works);
        if !ws.paused.is_empty() && ws.paused_fresh {
            doc.set("paused_fresh", true);
        }
        drop(ws);
        let tmp = self.data_dir.join("session.json.tmp");
        if std::fs::write(&tmp, doc.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, self.data_dir.join("session.json"));
        }
    }

    fn restore_session(&self) {
        let Some(text) = std::fs::read_to_string(self.data_dir.join("session.json")).ok() else { return };
        // файлы из архивов Moodle лежали в старой папке данных `ru.pspu.proverka` — после переноса
        // пути в сессии указывают туда; заменяем на новую папку
        let text = rewrite_old_dir(text, &self.data_dir);
        let Some(doc) = json::parse(&text).ok() else { return };
        let mut kinds: TaskMeta = HashMap::new();
        let mut tpaths = Vec::new();
        for t in doc.get("tasks").as_arr() {
            let p = t.get("path").str_or("");
            kinds.insert(p.clone(), (t.get("kind").str_or("task"), t.get("scope").str_or(""), t.get("code").str_or("")));
            tpaths.push(p);
        }
        self.add_tasks(&tpaths, Some(&kinds), "");
        let mut meta: HashMap<String, (String, Option<String>)> = HashMap::new();
        let mut wpaths = Vec::new();
        let mut saved_out: HashMap<String, Json> = HashMap::new();
        let mut students: HashMap<String, String> = HashMap::new();
        let mut sources: HashMap<String, Vec<String>> = HashMap::new();
        let mut deadlines: HashMap<String, u64> = HashMap::new();
        let mut submitted: HashMap<String, u64> = HashMap::new();
        let mut paused: Vec<String> = Vec::new();
        for w in doc.get("works").as_arr() {
            let p = w.get("path").str_or("");
            if w.get("paused").bool_or(false) {
                paused.push(p.clone());
            }
            if let Some(d) = w.get("deadline").as_f64().filter(|d| *d > 0.0) {
                deadlines.insert(p.clone(), d as u64);
            }
            if let Some(d) = w.get("submitted").as_f64().filter(|d| *d > 0.0) {
                submitted.insert(p.clone(), d as u64);
            }
            let src = w.get("sources").str_list();
            if !src.is_empty() {
                sources.insert(p.clone(), src);
            }
            if let Some(s) = w.get("student").as_str() {
                students.insert(p.clone(), s.to_string());
            }
            meta.insert(p.clone(), (w.get("group").str_or(""), w.get("assignment").as_str().map(|s| s.to_string())));
            if !w.get("outcome").is_null() {
                saved_out.insert(p.clone(), w.get("outcome").clone());
            }
            wpaths.push(p);
        }
        self.add_works(&wpaths, Some(&meta), "");
        let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let mut repaired: Vec<String> = Vec::new();
        for w in ws.works.iter_mut() {
            if let Some(s) = students.get(&w.path) {
                w.student = s.clone();
            }
            if let Some(d) = deadlines.get(&w.path) {
                w.deadline = *d;
            }
            if let Some(d) = submitted.get(&w.path) {
                w.submitted = *d;
            }
            if let Some(src) = sources.get(&w.path) {
                self.attach_sources(w, src);
            }
            // ответы Moodle, загруженные старой версией: остальные файлы студента
            // (Excel, PowerPoint, PDF, фото) лежат в его папке — присоединяем
            let extra = moodle_siblings(&w.path);
            let missing: Vec<String> = extra.into_iter().filter(|x| !w.sources.contains(x)).collect();
            if !missing.is_empty() {
                self.attach_sources(w, &missing);
                repaired.push(w.path.clone());
            }
        }
        ws.paused = ws.works.iter().filter(|w| paused.contains(&w.path)).map(|w| w.id.clone()).collect();
        ws.paused_fresh = doc.get("paused_fresh").bool_or(false);
        let ids: Vec<(String, String)> = ws.works.iter().map(|w| (w.id.clone(), w.path.clone())).collect();
        for (id, path) in ids {
            if let Some(o) = saved_out.get(&path) {
                let mut o = Outcome::from_json(o);
                if repaired.contains(&path) && o.status == "done" {
                    let r = "Присоединены другие файлы студента из Moodle (Excel, PowerPoint…) — проверьте работу заново".to_string();
                    if !o.review.contains(&r) {
                        o.review.insert(0, r);
                    }
                }
                ws.outcomes.insert(id, o);
            }
        }
    }

    // -------------------------------------------------------------- снимки для интерфейса

    pub fn work_json(&self, ws: &Workspace, w: &Work, with_text: bool) -> Json {
        let set = self.set_for_work(ws, w);
        let scan = self.scan(ws, w);
        let mut o = Json::obj()
            .with("id", w.id.as_str())
            .with("name", w.name.as_str())
            .with("path", w.path.as_str())
            .with("group", w.group.as_str())
            .with("assignment", set.map(|s| s.code.clone()))
            .with("assignment_id", set.map(|s| s.id.clone()))
            .with("assignment_scope", set.map(|s| s.scope.clone()))
            .with("program", self.program_of(&w.group))
            .with("assignment_foreign", set.map(|s| w.assignment.is_none() && !self.set_fits(s, &w.group, &self.program_of(&w.group))).unwrap_or(false))
            .with("assignment_manual", w.assignment.clone())
            .with("deadline", w.deadline)
            .with("deadline_plan", if w.deadline == 0 { self.deadline_of(ws, w) } else { 0 })
            .with("submitted_at", w.submitted_at(&self.data_dir))
            .with("late", { let mut w2 = w.clone(); w2.deadline = self.deadline_of(ws, w); w2.late(&self.data_dir).is_some() })
            .with("variant", scan.variant_shown())
            .with("variant_checked", scan.variant_used.clone())
            .with("variant_source", scan.variant_source.as_str())
            .with("variant_notes", scan.variant_notes.clone())
            .with("variant_in_text", scan.variant_in_text.clone())
            .with("variant_in_name", scan.variant_in_name.clone())
            .with("words", scan.word_count)
            .with("images", w.images)
            .with("hidden_count", w.hidden.len())
            .with("notes", w.notes.clone())
            .with("needs_pdf", w.needs_pdf && w.visible.is_empty())
            .with("pages", w.pages.len())
            .with("links", w.all_urls())
            .with("transcribed", w.transcribed)
            .with("pdf_scan", w.pages.iter().any(|p| p.src == w.path && ext_of(&p.src) == "pdf"))
            .with("doc_photos", w.pages.iter().any(|p| p.src == w.path && !docx::is_image(&p.src) && ext_of(&p.src) != "pdf"))
            .with("prep", Json::Arr(w.pages.iter().enumerate().filter(|(_, p)| !p.ready).map(|(i, p)| Json::obj().with("i", i).with("name", file_name(&p.src).as_str())).collect()))
            .with("sources", Json::Arr(w.sources.iter().map(|x| Json::from(file_name(x))).collect()))
            .with("source_paths", w.sources.clone())
            .with("missing_files", scan.missing_files.clone())
            .with("required_files", scan.required_files.clone())
            .with("excel_findings", scan.excel_findings.clone())
            .with("excel_ok", scan.excel_ok.clone())
            .with("pdf_sources", Json::Arr(w.pending_pdf.iter().enumerate().map(|(i, x)| Json::obj().with("i", i).with("name", file_name(x).as_str())).collect()))
            .with("strong", scan.strong_count())
            .with("evidence_count", scan.evidence.len())
            .with("size", w.size);
        if with_text {
            o.set("text", w.visible.as_str());
            o.set("page_list", Json::Arr(w.pages.iter().enumerate().map(|(i, p)| Json::obj().with("i", i).with("name", file_name(&p.src).as_str()).with("src", p.src.as_str()).with("ext", ext_of(&p.file).as_str())).collect()));
            o.set("hidden", w.hidden.clone());
            o.set("scan", scan.to_json());
            o.set(
                "plag",
                Json::Arr(
                    ws.plag
                        .iter()
                        .filter_map(|(a, b, s)| {
                            let other = if a == &w.id { b } else if b == &w.id { a } else { return None };
                            let x = ws.works.iter().find(|x| &x.id == other)?;
                            Some(Json::obj().with("id", x.id.as_str()).with("name", format!("{}{}", if x.group.is_empty() { String::new() } else { format!("{}/", x.group) }, x.name)).with("score", (s * 100.0).round()))
                        })
                        .collect(),
                ),
            );
        }
        let (authors, in_roster) = self.authors(ws, w);
        o.set("student", authors.join(" и "));
        o.set("authors", authors.clone());
        o.set("student_in_roster", in_roster);
        let twin: Vec<String> = ws
            .team_dups
            .iter()
            .filter_map(|(a, b)| if a == &w.id { Some(b) } else if b == &w.id { Some(a) } else { None })
            .filter_map(|id| ws.works.iter().find(|x| &x.id == id).map(|x| x.name.clone()))
            .collect();
        o.set("team_twin", twin);
        o.set(
            "archive_hits",
            Json::Arr(ws.archive_hits.get(&w.id).map(|v| v.iter().map(|(l, sc)| Json::obj().with("who", l.as_str()).with("score", *sc)).collect()).unwrap_or_default()),
        );
        let oc = ws.outcomes.get(&w.id).cloned().unwrap_or_default();
        o.set("outcome", oc.to_json(&w.id, &w.path));
        o
    }

    pub fn state_json(&self) -> Json {
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks = Json::arr();
        for t in &ws.tasks {
            tasks.push(
                Json::obj()
                    .with("id", t.id.as_str())
                    .with("name", t.name.as_str())
                    .with("path", t.path.as_str())
                    .with("kind", t.kind.as_str())
                    .with("code", t.code.as_str())
                    .with("needs_pdf", t.needs_pdf && t.marked.trim().is_empty())
                    .with("scope", t.scope.as_str())
                    .with("hidden_count", docx::hidden_of(&t.marked).len())
                    .with("chars", t.marked.chars().count()),
            );
        }
        let sets = Json::Arr(ws.sets.iter().map(|s| s.to_json(false)).collect());
        let works = Json::Arr(ws.works.iter().map(|w| self.work_json(&ws, w, false)).collect());
        let plag = Json::Arr(
            ws.plag
                .iter()
                .map(|(a, b, s)| Json::obj().with("a", a.as_str()).with("b", b.as_str()).with("score", (s * 100.0).round()))
                .collect(),
        );
        Json::obj()
            .with("tasks", tasks)
            .with("sets", sets)
            .with("works", works)
            .with("plag", plag)
            .with("running", self.running.load(Ordering::SeqCst))
            .with("pausing", self.pause.load(Ordering::SeqCst) && self.running.load(Ordering::SeqCst))
            .with("paused", ws.paused.clone())
            .with("settings", self.settings())
            .with("groups", self.groups_json(&ws))
            .with("data_dir", self.data_dir.to_string_lossy().to_string())
    }
}

/// Естественная сортировка: «Вариант 2» раньше «Вариант 10».
pub fn natural_key(s: &str) -> Vec<(u8, u64, String)> {
    let mut out = Vec::new();
    let mut num = String::new();
    let mut txt = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !txt.is_empty() {
                out.push((1, 0, std::mem::take(&mut txt)));
            }
            num.push(c);
        } else {
            if !num.is_empty() {
                out.push((0, num.parse().unwrap_or(0), String::new()));
                num.clear();
            }
            txt.push(c);
        }
    }
    if !num.is_empty() {
        out.push((0, num.parse().unwrap_or(0), String::new()));
    }
    if !txt.is_empty() {
        out.push((1, 0, txt));
    }
    out
}

// ------------------------------------------------------------------ двойная проверка, журнал, отзывы

impl Core {
    /// Поводы для преподавателя взглянуть на работу самому.
    pub fn review_flags(&self, scan: &Scan, llm: &Json, violation: &str) -> Vec<String> {
        let mut r = Vec::new();
        let thr = self.settings().get("penalties").get("ai_confidence").f64_or(0.7);
        let conf = {
            let c = llm.get("ai_confidence").f64_or(0.0);
            if c > 1.0 {
                c / 100.0
            } else {
                c
            }
        };
        if violation.is_empty() {
            let medium = scan.evidence.iter().filter(|e| e.weight == "medium").count();
            if llm.get("ai_detected").bool_or(false) && conf < thr && !llm.get("ai_evidence").str_list().is_empty() {
                r.push(format!("Нейросеть подозревает ИИ, но не уверена ({} %) — стоит спросить устно", (conf * 100.0).round()));
            } else if medium > 0 && !llm.get("ai_detected").bool_or(false) {
                r.push(format!("Найдены средние признаки ИИ ({}), но нейросеть их не подтвердила", medium));
            }
        }
        r
    }

    /// Мнение ещё одной модели: копится в `llm.opinions` (первое — также в
    /// `second_opinion`). При расхождении работа помечается «нужен ваш взгляд»,
    /// по настройке итоговая оценка считается по всем моделям.
    pub fn attach_second_opinion(&self, wid: &str, j: &Json, model: &str) {
        let st = self.settings();
        let dc = st.get("double_check").clone();
        let max_diff = dc.get("max_diff").f64_or(2.0).max(0.5);
        let policy = dc.get("policy").str_or("primary");
        let thr = st.get("penalties").get("ai_confidence").f64_or(0.7);
        let half = st.get("half_grades").bool_or(true);
        let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let Some(w) = ws.works.iter().find(|w| w.id == wid).cloned() else { return };
        let scan = self.scan(&ws, &w);
        let Some(o) = ws.outcomes.get_mut(wid) else { return };
        let g1 = o.llm.get("grade").f64_or(0.0);
        let g2 = norm_grade(j.get("grade").f64_or(2.0), half);
        let conf = {
            let c = j.get("ai_confidence").f64_or(0.0);
            if c > 1.0 { c / 100.0 } else { c }
        };
        // довод модели об ИИ проверяем по тексту работы
        let (j_ok, _) = Core::verify_model_evidence(&w, j);
        let j_ai = j.get("ai_detected").bool_or(false) && conf >= thr && (!j_ok.is_empty() || scan.evidence.iter().any(|e| e.weight != "weak"));
        let op = Json::obj()
            .with("model", model)
            .with("grade", g2)
            .with("grade_text", fmt_grade(g2))
            .with("completeness", j.get("completeness").i64_or(0))
            .with("ai_detected", j_ai)
            .with("ai_confidence", j.get("ai_confidence").f64_or(0.0))
            .with("summary", j.get("summary").str_or(""));
        let mut ops: Vec<Json> = o.llm.get("opinions").as_arr().to_vec();
        if ops.is_empty() && !o.llm.get("second_opinion").is_null() {
            ops.push(o.llm.get("second_opinion").clone());
        }
        ops.retain(|x| x.get("model").str_or("") != model);
        ops.push(op);
        o.llm.set("second_opinion", ops[0].clone());
        o.llm.set("opinions", Json::Arr(ops.clone()));

        // расхождения — пересобираем пометки целиком
        o.review.retain(|r| !r.starts_with("Модели разошлись") && !r.starts_with("Модели расходятся во мнении об ИИ"));
        let mut grades: Vec<f64> = if g1 > 0.0 { vec![g1] } else { Vec::new() };
        grades.extend(ops.iter().map(|x| x.get("grade").f64_or(0.0)).filter(|x| *x > 0.0));
        let lo = grades.iter().cloned().fold(f64::MAX, f64::min);
        let hi = grades.iter().cloned().fold(f64::MIN, f64::max);
        if grades.len() > 1 && hi - lo >= max_diff - 0.01 {
            o.review.push(format!(
                "Модели разошлись: основная — «{}», {}",
                fmt_grade(g1),
                ops.iter().map(|x| format!("{} — «{}»", x.get("model").str_or(""), x.get("grade_text").str_or(""))).collect::<Vec<_>>().join(", ")
            ));
        }
        let o_ai = !o.ai_suspect.is_null() || o.violation == "ai";
        let ai_yes = ops.iter().filter(|x| x.get("ai_detected").bool_or(false)).count();
        if (o_ai && ai_yes < ops.len()) || (!o_ai && ai_yes > 0) {
            o.review.push(format!(
                "Модели расходятся во мнении об ИИ: основная — {}, из остальных {} из {} видят признаки ИИ",
                if o_ai { "да" } else { "нет" },
                ai_yes,
                ops.len()
            ));
        }
        // итоговая оценка по всем моделям
        if g1 > 0.0 && policy != "primary" {
            let mut s = grades.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let merged = match policy.as_str() {
                "min" => s[0],
                "max" => s[s.len() - 1],
                "median" => {
                    let n = s.len();
                    if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 }
                }
                _ => s.iter().sum::<f64>() / s.len() as f64,
            };
            let merged = norm_grade(merged, half);
            o.llm.set("merged_grade", merged);
            drop(ws);
            let _ = self.refinalize(wid);
            let mut ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(o) = ws.outcomes.get_mut(wid) {
                let how = match policy.as_str() {
                    "min" => "меньшая",
                    "max" => "большая",
                    "median" => "медиана",
                    _ => "среднее",
                };
                o.reasons.push(format!("Оценка по {} моделям ({}): {}", grades.len(), how, grades.iter().map(|g| format!("«{}»", fmt_grade(*g))).collect::<Vec<_>>().join(", ")));
            }
        }
    }

    /// Кто автор работы (по списку группы, имени файла или варианту).
    /// Резкий скачок стиля по сравнению с прошлыми работами того же студента (из журнала).
    fn style_check(&self, ws: &Workspace, w: &Work, sc: &mut Scan) {
        if !self.settings().get("penalties").get("style_jump").bool_or(true) {
            return;
        }
        let cur = detect::style_features(&w.visible);
        if cur.is_empty() {
            return;
        }
        let code = self.set_for_work(ws, w).map(|s| s.code.clone()).unwrap_or_default();
        let j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        // без вызова scan: ФИО — из Moodle или по фамилии в имени файла / варианту
        let student = if !w.student.is_empty() {
            j.rosters.get(&w.group).and_then(|l| l.iter().find(|n| super::moodle::same_person(n, &w.student)).cloned()).unwrap_or_else(|| w.student.clone())
        } else {
            let v = sc.variant_in_name.clone().or(sc.variant_in_text.clone());
            match j.student_for(&w.group, &w.name, v.as_deref()) {
                (s, true) => s,
                _ => return,
            }
        };
        let prev: Vec<Vec<f64>> = j.entries.iter().filter(|e| e.group == w.group && e.student == student && e.code != code && !e.style.is_empty() && e.violation.is_empty()).map(|e| e.style.clone()).collect();
        drop(j);
        if let Some((dist, what)) = detect::style_jump(&cur, &prev) {
            if dist >= 2.3 && !what.is_empty() {
                sc.evidence.push(detect::Evidence {
                    kind: "style_jump".into(),
                    weight: if dist >= 3.2 { "medium".into() } else { "weak".into() },
                    what: format!("Стиль резко отличается от прошлых работ студента ({} шт.): {}", prev.len(), what.join("; ")),
                    snippet: String::new(),
                });
            }
        }
    }

    /// Вариант, назначенный студенту в списке группы: (ФИО, вариант).
    pub fn roster_variant(&self, w: &Work) -> Option<(String, String)> {
        let j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let map = j.assigned.get(&w.group).filter(|m| !m.is_empty())?;
        let student = if !w.student.is_empty() {
            j.rosters.get(&w.group).and_then(|l| l.iter().find(|n| super::moodle::same_person(n, &w.student)).cloned()).unwrap_or_else(|| w.student.clone())
        } else {
            match j.student_for(&w.group, &w.name, None) {
                (s, true) => s,
                _ => return None,
            }
        };
        map.get(&student).filter(|v| !v.is_empty()).map(|v| (student.clone(), v.clone()))
    }

    /// ФИО для показа: при работе в паре — оба студента («Лесовиков Иван и Громобоева Анна»).
    pub fn student_of(&self, ws: &Workspace, w: &Work) -> (String, bool) {
        let (list, ok) = self.authors(ws, w);
        (list.join(" и "), ok)
    }

    /// Авторы работы: тот, кто сдал (или чья фамилия в имени файла), плюс напарники —
    /// по парам из списка группы и по фамилиям одногруппников в имени файла и в начале работы.
    pub fn authors(&self, ws: &Workspace, w: &Work) -> (Vec<String>, bool) {
        let (main, in_roster) = self.main_student(ws, w);
        let j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let roster = match j.rosters.get(&w.group) {
            Some(r) if !r.is_empty() => r.clone(),
            _ => return (vec![main], in_roster),
        };
        let mut found: Vec<String> = Vec::new();
        // фамилии одногруппников в имени файла (в том числе латиницей) и на титульном листе
        // титульные строки: первые три и строки «Выполнили: …», «Студенты: …», «ФИО: …»
        let head: String = w
            .visible
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(25)
            .enumerate()
            .filter(|(i, l)| {
                let low = l.to_lowercase();
                *i < 3 || ["выполн", "студент", "подготов", "автор", "фио", "ф.и.о"].iter().any(|k| low.contains(k))
            })
            .map(|(_, l)| l)
            .collect::<Vec<_>>()
            .join(" ");
        let hay = super::roster::tokens(&format!("{} {}", w.name, head));
        for n in &roster {
            if super::roster::mentioned(n, &roster, &hay) {
                found.push(n.clone());
            }
        }
        let mut out: Vec<String> = Vec::new();
        if in_roster {
            out.push(main.clone());
        } else if found.is_empty() {
            return (vec![main], in_roster);
        }
        for n in found {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        // напарники, назначенные в списке группы
        if let Some(t) = out.first().and_then(|m| j.team_of(&w.group, m)) {
            for n in &roster {
                if !out.contains(n) && j.team_of(&w.group, n).as_deref() == Some(t.as_str()) {
                    out.push(n.clone());
                }
            }
        }
        if out.is_empty() {
            return (vec![main], in_roster);
        }
        (out, true)
    }

    /// Кто сдал работу (один студент).
    pub fn main_student(&self, ws: &Workspace, w: &Work) -> (String, bool) {
        if !w.student.is_empty() {
            // ФИО известно (Moodle): если студент есть в списке группы — берём запись из списка
            let j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(n) = j.rosters.get(&w.group).and_then(|l| l.iter().find(|n| super::moodle::same_person(n, &w.student))) {
                return (n.clone(), true);
            }
            return (w.student.clone(), true);
        }
        let scan = self.scan(ws, w);
        let v = scan.variant_in_name.clone().or(scan.variant_in_text.clone());
        self.journal.lock().unwrap_or_else(|e| e.into_inner()).student_for(&w.group, &w.name, v.as_deref())
    }

    /// Записывает текущие оценки в журнал группы. Возвращает число записей.
    pub fn journal_add_current(&self) -> usize {
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = Vec::new();
        let mut topics: Vec<(String, String, String, String)> = Vec::new();
        let mut cal: Vec<super::history::CalRec> = Vec::new();
        let mut arc: Vec<super::history::ArcEntry> = Vec::new();
        let arch_on = self.settings().get("archive").get("enabled").bool_or(true);
        let task_sh: HashSet<u64> = ws.sets.iter().flat_map(|s| s.variants.values().map(|v| format!("{}\n{}", v.task_visible, v.key))).flat_map(|t| detect::shingle_set(&t)).collect();
        for w in &ws.works {
            let Some(o) = ws.outcomes.get(&w.id) else { continue };
            let Some(g) = o.final_grade() else { continue };
            let set = self.set_for_work(&ws, w);
            let code = set.map(|s| s.code.clone()).unwrap_or_else(|| "Работа".into());
            // работа без группы — в группу «Без группы», а не в безымянную вкладку журнала
            let wgroup = if w.group.trim().is_empty() { "Без группы".to_string() } else { w.group.clone() };
            let title = set.map(|s| s.title.clone()).unwrap_or_default();
            // сверка: оценка приложения против итоговой (только работы, проверенные нейросетью)
            if let Some(auto) = o.grade {
                if !o.llm.is_null() {
                    cal.push(super::history::CalRec { key: format!("{:x}", w.hash), code: code.clone(), group: wgroup.clone(), model: o.model.clone(), auto, final_: g, date: if o.checked_at > 0 { o.checked_at } else { now_s() } });
                }
            }
            // архив: отпечатки текста для сравнения с работами следующих лет
            if arch_on && w.visible.split_whitespace().count() > 40 {
                let (who, _) = self.authors(&ws, w);
                arc.push(super::history::ArcEntry {
                    code: code.clone(),
                    title: title.clone(),
                    group: wgroup.clone(),
                    student: who.join(", "),
                    date: if o.checked_at > 0 { o.checked_at } else { now_s() },
                    hash: format!("{:x}", w.hash),
                    shingles: detect::shingle_set(&w.visible).difference(&task_sh).copied().collect(),
                    ..Default::default()
                });
            }
            let subject = set.map(|s| self.settings().get("set_subjects").get(&s.id).str_or("")).unwrap_or_default();
            topics.push((wgroup.clone(), code.clone(), title, subject));
            let (students, _) = self.authors(&ws, w);
            let scan = self.scan(&ws, w);
            // работа в паре — оценка каждому из напарников
            for student in students {
                entries.push(Entry {
                    group: wgroup.clone(),
                    student,
                    code: code.clone(),
                    grade: Some(g),
                    violation: o.violation.clone(),
                    date: if o.checked_at > 0 { o.checked_at } else { now_s() },
                    file: w.name.clone(),
                    variant: scan.variant_shown().unwrap_or_default(),
                    comment: o.override_comment.clone(),
                    style: detect::style_features(&w.visible),
                    flag: String::new(),
                });
            }
        }
        drop(ws);
        super::history::cal_add(&self.data_dir, cal);
        let added = super::history::arc_add(&self.data_dir, arc);
        if added > 0 {
            self.log(&format!("Архив прошлых лет: добавлено работ {}", added));
        }
        let n = entries.len();
        let mut j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        for e in entries {
            j.upsert(e);
        }
        for (g, c, t, sub) in topics {
            j.ensure_group(&g);
            j.touch_topic(&g, &c, "", &t, &sub);
        }
        j.save();
        n
    }

    /// Срок сдачи работы: свой (из Moodle или «Срок сдачи…») или из календаря журнала группы.
    pub fn deadline_of(&self, ws: &Workspace, w: &Work) -> u64 {
        if w.deadline > 0 {
            return w.deadline;
        }
        let Some(code) = self.set_for_work(ws, w).map(|s| s.code.clone()) else { return 0 };
        let t = self.journal.lock().unwrap_or_else(|e| e.into_inner()).topic(&w.group, &code);
        super::journal::parse_local(&t.deadline).map(|x| (x - super::report::TZ_MIN.load(Ordering::Relaxed) * 60).max(0) as u64).unwrap_or(0)
    }

    /// Отзыв студенту по результатам проверки (без нейросети).
    pub fn feedback_template(&self, wid: &str) -> Option<String> {
        let ws = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let w = ws.works.iter().find(|w| w.id == wid)?;
        let o = ws.outcomes.get(wid)?;
        let (student, in_roster) = self.student_of(&ws, w);
        let code = self.set_for_work(&ws, w).map(|s| format!("{} «{}»", s.code, s.title)).unwrap_or_else(|| "практическая работа".into());
        let variant = self.scan(&ws, w).variant_shown().unwrap_or_default();
        let mut t = String::new();
        if in_roster || !student.starts_with("Вариант") {
            let first = student.split_whitespace().nth(1).unwrap_or(&student);
            t.push_str(&format!("{}, здравствуйте!\n\n", first));
        } else {
            t.push_str("Здравствуйте!\n\n");
        }
        t.push_str(&format!("Работа: {}{}.\n", code, if variant.is_empty() { String::new() } else { format!(", вариант {}", variant) }));
        match o.final_grade() {
            Some(g) => t.push_str(&format!("Оценка: «{}».\n\n", fmt_grade(g))),
            None => t.push_str("Работа ещё не оценена.\n\n"),
        }
        match o.violation.as_str() {
            "ai" => {
                t.push_str("В работе обнаружены признаки выполнения с помощью нейросети");
                let ev: Vec<String> = o.ai_evidence.iter().take(3).cloned().collect();
                if !ev.is_empty() {
                    t.push_str(&format!(" ({})", ev.join("; ")));
                }
                t.push_str(". Поэтому работа оценена на «2». Если вы выполняли её самостоятельно, подойдите на устную защиту: объясните свои ответы, и оценка будет пересмотрена.\n\n");
            }
            "plagiarism" => {
                let who: Vec<String> = o.plag.iter().map(|(n, _)| n.clone()).collect();
                t.push_str(&format!("Текст работы почти совпадает с другой работой{}. Поэтому работа оценена на «2». Если работа выполнена самостоятельно, подойдите на устную защиту.\n\n", if who.is_empty() { String::new() } else { format!(" ({})", who.join(", ")) }));
            }
            _ => {}
        }
        // баллы по критериям рубрики
        let rub = o.llm.get("rubric").as_arr().to_vec();
        if !rub.is_empty() && !o.llm.get("rubric_total").is_null() {
            t.push_str(&format!("Баллы по критериям ({} из {}):\n", fmt_grade(o.llm.get("rubric_total").f64_or(0.0)), fmt_grade(o.llm.get("rubric_max").f64_or(0.0))));
            for c in &rub {
                let cm = c.get("comment").str_or("");
                t.push_str(&format!("• {} — {} из {}{}\n", c.get("name").str_or(""), fmt_grade(c.get("points").f64_or(0.0)), fmt_grade(c.get("max").f64_or(0.0)), if cm.is_empty() { String::new() } else { format!(": {}", cm) }));
            }
            t.push('\n');
        }
        let tasks = o.llm.get("tasks").as_arr().to_vec();
        let good: Vec<String> = tasks.iter().filter(|x| x.get("status").str_or("") == "верно").map(|x| x.get("n").str_or("")).collect();
        let bad: Vec<&Json> = tasks.iter().filter(|x| x.get("status").str_or("") != "верно").collect();
        if o.violation.is_empty() {
            if !good.is_empty() {
                t.push_str(&format!("Что получилось хорошо: задания {} выполнены верно.\n\n", good.join(", ")));
            }
            if !bad.is_empty() {
                t.push_str("Над чем поработать:\n");
                for x in bad {
                    t.push_str(&format!("• Задание {} ({}): {}\n", x.get("n").str_or(""), x.get("status").str_or(""), x.get("comment").str_or("")));
                }
                t.push('\n');
            }
            let errs = o.llm.get("errors").str_list();
            if !errs.is_empty() {
                t.push_str("Ошибки, на которые стоит обратить внимание:\n");
                for e in errs.iter().take(6) {
                    t.push_str(&format!("• {}\n", e));
                }
                t.push('\n');
            }
        }
        let caps: Vec<&String> = o.reasons.iter().filter(|r| r.contains("максимум") || r.contains("менее трети")).collect();
        if !caps.is_empty() && o.violation.is_empty() {
            t.push_str("Почему оценка ограничена: ");
            t.push_str(&caps.iter().map(|s| s.trim_end_matches('.').to_string()).collect::<Vec<_>>().join("; "));
            t.push_str(".\n\n");
        }
        let summary = o.llm.get("summary").str_or("");
        if !summary.is_empty() && o.violation.is_empty() {
            t.push_str(&format!("Общий вывод: {}\n", summary));
        }
        Some(t.trim().to_string())
    }
}

impl Core {
    /// Все известные группы: зарегистрированные, из списков студентов, из папок работ и из «для кого» заданий.
    pub fn groups_json(&self, ws: &Workspace) -> Json {
        let j = self.journal.lock().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<String> = j.groups.iter().map(|g| g.name.clone()).collect();
        let add = |n: &str, names: &mut Vec<String>| {
            if !n.trim().is_empty() && !names.iter().any(|x| x == n) {
                names.push(n.to_string());
            }
        };
        for g in j.rosters.keys().chain(j.topics.keys()) {
            add(g, &mut names);
        }
        for e in &j.entries {
            add(&e.group, &mut names);
        }
        for w in &ws.works {
            add(&w.group, &mut names);
        }
        let programs = self.programs();
        for t in &ws.tasks {
            if !t.scope.is_empty() && !programs.contains(&t.scope) {
                add(&t.scope, &mut names);
            }
        }
        names.sort_by_key(|n| natural_key(n));
        Json::Arr(
            names
                .iter()
                .map(|n| {
                    let info = j.groups.iter().find(|g| &g.name == n);
                    Json::obj()
                        .with("name", n.as_str())
                        .with("program", info.map(|g| g.program.clone()).unwrap_or_default())
                        .with("note", info.map(|g| g.note.clone()).unwrap_or_default())
                        .with("registered", info.is_some())
                        .with("roster", j.rosters.get(n).map(|r| r.len()).unwrap_or(0))
                        .with("works", ws.works.iter().filter(|w| &w.group == n).count())
                        .with("grades", j.entries.iter().filter(|e| &e.group == n).count())
                })
                .collect(),
        )
    }

    /// Направления обучения (настраиваемый список).
    pub fn programs(&self) -> Vec<String> {
        let mut p = self.settings().get("programs").str_list();
        if p.is_empty() {
            p = vec!["СПО-9".into(), "СПО-11".into(), "Специалитет".into(), "Бакалавриат".into(), "Магистратура".into()];
        }
        p
    }
}

#[cfg(test)]
mod data_dir_tests {
    use super::*;
    #[test]
    fn migrates_old_folder() {
        let base = std::env::temp_dir().join(format!("proverka-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join("ru.pspu.proverka");
        std::fs::create_dir_all(old.join("moodle")).unwrap();
        let p = old.join("moodle").join("a.docx").to_string_lossy().to_string();
        std::fs::write(old.join("session.json"), Json::obj().with("works", Json::Arr(vec![Json::obj().with("path", p.as_str())])).to_string()).unwrap();
        let new = base.join("proverka");
        migrate_data_dir(&old, &new);
        assert!(new.join("session.json").exists() && !old.exists());
        let t = rewrite_old_dir(std::fs::read_to_string(new.join("session.json")).unwrap(), &new);
        assert!(t.contains("proverka") && !t.contains("ru.pspu.proverka"), "{}", t);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod ymd_tests {
    #[test]
    fn dates() {
        assert_eq!(super::ymd(0), "1970-01-01");
        assert_eq!(super::ymd(1795424700), "2026-11-23");
        assert_eq!(super::ymd(951782400), "2000-02-29");
    }
}

// ------------------------------------------------------------------ рубрика

/// Критерий рубрики: «Оформление — до 1 балла».
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crit {
    pub name: String,
    pub max: f64,
    pub desc: String,
}

/// Рубрика набора заданий: критерии и пороги оценок в процентах от максимума.
#[derive(Clone, Debug, Default)]
pub struct Rubric {
    pub items: Vec<Crit>,
    /// пороги: «5» от p5 %, «4» от p4 %, «3» от p3 %, ниже — «2»
    pub p5: f64,
    pub p4: f64,
    pub p3: f64,
}

impl Rubric {
    pub fn from_json(j: &Json) -> Rubric {
        let items: Vec<Crit> = j
            .get("items")
            .as_arr()
            .iter()
            .map(|c| Crit { name: c.get("name").str_or("").trim().to_string(), max: c.get("max").f64_or(0.0).max(0.0), desc: c.get("desc").str_or("").trim().to_string() })
            .filter(|c| !c.name.is_empty() && c.max > 0.0)
            .collect();
        let sc = j.get("scale");
        Rubric { items, p5: sc.get("5").f64_or(85.0), p4: sc.get("4").f64_or(70.0), p3: sc.get("3").f64_or(50.0) }
    }

    pub fn total_max(&self) -> f64 {
        self.items.iter().map(|c| c.max).sum()
    }

    /// Оценка по проценту набранных баллов.
    pub fn grade_of(&self, total: f64, half: bool) -> f64 {
        let max = self.total_max();
        if max <= 0.0 {
            return 2.0;
        }
        let pct = total / max * 100.0;
        let g = if pct >= self.p5 {
            5.0
        } else if pct >= self.p4 {
            4.0
        } else if pct >= self.p3 {
            3.0
        } else {
            2.0
        };
        // с половинками: между порогами — «с половиной», если набрано больше середины промежутка
        if half && g < 5.0 {
            let (lo, hi) = match g as i64 {
                4 => (self.p4, self.p5),
                3 => (self.p3, self.p4),
                _ => (0.0, self.p3),
            };
            if g >= 3.0 && hi > lo && pct >= lo + (hi - lo) * 0.5 {
                return g + 0.5;
            }
        }
        g
    }

    /// Текст для нейросети: критерии и формат ответа.
    pub fn prompt(&self) -> String {
        let mut s = format!("РУБРИКА ПРЕПОДАВАТЕЛЯ (оценивай строго по ней, максимум {} баллов):\n", fmt_grade(self.total_max()));
        for (i, c) in self.items.iter().enumerate() {
            s.push_str(&format!("{}) {} — до {} балл.{}\n", i + 1, c.name, fmt_grade(c.max), if c.desc.is_empty() { String::new() } else { format!(" {}", c.desc) }));
        }
        s.push_str("Поставь баллы по каждому критерию (можно дробные, не больше максимума критерия) и добавь в ответ поле \"rubric\": [{\"name\": \"название критерия\", \"points\": число, \"comment\": \"почему столько, коротко\"}] — по всем критериям в том же порядке. Итоговую оценку приложение посчитает по сумме баллов.");
        s
    }

    /// Баллы модели → нормализованная рубрика в ответе (`rubric`, `rubric_total`, `rubric_max`)
    /// и оценка по сумме (`grade`; прежняя оценка модели — в `model_grade`). Нет рубрики — false.
    pub fn apply(&self, llm: &mut Json, half: bool) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let given = llm.get("rubric").as_arr().to_vec();
        let low = |s: &str| s.to_lowercase().replace('ё', "е");
        let mut out: Vec<Json> = Vec::new();
        let mut total = 0.0;
        let mut answered = 0;
        for (i, c) in self.items.iter().enumerate() {
            // по названию, иначе по порядку
            let g = given.iter().find(|x| low(&x.get("name").str_or("")) == low(&c.name)).or_else(|| given.get(i));
            let pts = g.and_then(|x| x.get("points").as_f64().or_else(|| x.get("points").str_or("").replace(',', ".").parse::<f64>().ok()));
            if pts.is_some() {
                answered += 1;
            }
            let cm = g.map(|x| x.get("comment").str_or("")).unwrap_or_default();
            match pts {
                Some(v) => {
                    let v = v.clamp(0.0, c.max);
                    total += v;
                    out.push(Json::obj().with("name", c.name.as_str()).with("max", c.max).with("points", (v * 100.0).round() / 100.0).with("comment", cm.as_str()));
                }
                // балла нет — так и храним (null), чтобы при пересчёте не принять за «0»
                None => out.push(Json::obj().with("name", c.name.as_str()).with("max", c.max).with("points", Json::Null).with("comment", cm.as_str())),
            }
        }
        if answered == 0 {
            // модель рубрику не заполнила — оценку не трогаем, но покажем пустую рубрику
            llm.set("rubric", Json::Arr(out));
            llm.set("rubric_total", Json::Null);
            llm.set("rubric_max", self.total_max());
            llm.set("rubric_grade", Json::Null);
            return true;
        }
        let grade = self.grade_of(total, half);
        if llm.get("model_grade").is_null() {
            let mg = llm.get("grade").clone();
            llm.set("model_grade", mg);
        }
        llm.set("rubric", Json::Arr(out));
        llm.set("rubric_total", (total * 100.0).round() / 100.0);
        llm.set("rubric_max", self.total_max());
        llm.set("rubric_grade", grade);
        llm.set("grade", grade);
        true
    }
}

#[cfg(test)]
mod rubric_tests {
    use super::*;

    #[test]
    fn rubric_grade() {
        let r = Rubric::from_json(&json::parse(r#"{"items":[{"name":"Расчёты","max":3},{"name":"Оформление","max":1},{"name":"Выводы","max":1}],"scale":{"5":85,"4":70,"3":50}}"#).unwrap());
        assert_eq!(r.total_max(), 5.0);
        assert_eq!(r.grade_of(5.0, true), 5.0);
        assert_eq!(r.grade_of(3.5, false), 4.0); // 70 %
        assert_eq!(r.grade_of(4.0, true), 4.5); // 80 % — выше середины между 70 и 85
        assert_eq!(r.grade_of(2.0, true), 2.0);
        let mut llm = json::parse(r#"{"grade":5,"rubric":[{"name":"Расчёты","points":2,"comment":"ошибка в маске"},{"name":"оформление","points":1},{"name":"Выводы","points":7}]}"#).unwrap();
        assert!(r.apply(&mut llm, true));
        assert_eq!(llm.get("rubric_total").as_f64(), Some(4.0)); // «Выводы» обрезаны до максимума 1
        assert_eq!(llm.get("grade").as_f64(), Some(4.5));
        assert_eq!(llm.get("model_grade").as_f64(), Some(5.0));
        assert!(!Rubric::default().apply(&mut llm, true));
    }
}
