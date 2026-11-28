//! Скорость модели и видеопамять.
//!
//! * Для каждой модели (и числа работ одновременно) запоминается обычная скорость ответа —
//!   медиана последних запросов (`speed.json`). Если в текущей проверке модель отвечает
//!   в 3 раза медленнее обычного, приложение предупреждает: обычно это значит, что модель
//!   не поместилась в видеопамять или LM Studio сменил движок (CUDA → Vulkan).
//! * Видеопамять (nvidia-smi, если есть) — для карточки скорости и для пояснения в
//!   предупреждении о скорости. Сама по себе почти полная видеопамять — норма (модель
//!   загружена), поэтому отдельно о ней приложение не предупреждает.

use super::core::Core;
use super::json::{self, Json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Запросы с меньшим ответом не показательны: в них почти всё время — чтение запроса.
const MIN_TOKENS: i64 = 80;
/// Во сколько раз медленнее обычного — повод предупредить.
const SLOW: f64 = 3.0;
/// Сколько прошлых замеров нужно, чтобы знать «обычную» скорость.
const MIN_HISTORY: usize = 5;

#[derive(Default)]
struct RunPerf {
    conc: usize,
    local: HashMap<String, bool>,
    samples: HashMap<String, Vec<f64>>,
    warned: HashSet<String>,
}

static RUN: Mutex<Option<RunPerf>> = Mutex::new(None);

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 }
}

fn history_load(dir: &Path) -> HashMap<String, Vec<f64>> {
    let Ok(t) = std::fs::read_to_string(dir.join("speed.json")) else { return HashMap::new() };
    let Ok(j) = json::parse(&t) else { return HashMap::new() };
    match &j {
        Json::Obj(m) => m.iter().map(|(k, v)| (k.clone(), v.as_arr().iter().filter_map(|x| x.as_f64()).collect())).collect(),
        _ => HashMap::new(),
    }
}

fn history_save(dir: &Path, h: &HashMap<String, Vec<f64>>) {
    let mut o = Json::obj();
    let mut keys: Vec<&String> = h.keys().collect();
    keys.sort();
    for k in keys {
        o.set(k, Json::Arr(h[k].iter().map(|x| Json::Num((x * 10.0).round() / 10.0)).collect()));
    }
    let _ = std::fs::write(dir.join("speed.json"), o.to_string());
}

/// 15974 МБ → «15,6»
fn gb(mb: u64) -> String {
    format!("{:.1}", mb as f64 / 1024.0).replace('.', ",")
}

fn key(model: &str, conc: usize) -> String {
    format!("{}|{}", model, conc)
}

/// Предупреждать о скорости и видеопамяти (преподаватель может выключить).
fn warn_on(core: &Core) -> bool {
    core.settings().get("perf").get("warn").bool_or(true)
}

/// Начало проверки: сбросить замеры и посмотреть видеопамять (для локальной модели).
pub fn run_started(core: &Core) {
    let conc = core.settings().get("concurrency").i64_or(1).clamp(1, 8) as usize;
    *RUN.lock().unwrap_or_else(|e| e.into_inner()) = Some(RunPerf { conc, ..Default::default() });
    let local = super::grader::active_provider(core).map(|p| p.is_local()).unwrap_or(false);
    if !local {
        return;
    }
    // только в журнал: загруженная модель и должна занимать почти всю видеопамять —
    // само по себе это не беда; о проблеме говорит падение скорости (см. sample)
    if let Some(g) = gpu() {
        core.log(&format!("Видеопамять {}: занято {} из {} ГБ", g.name, gb(g.used_mb), gb(g.total_mb)));
    }
}

/// Замер одного запроса к модели.
pub fn sample(core: &Core, model: &str, local: bool, tokens_out: i64, secs: f64) {
    if tokens_out < MIN_TOKENS || secs <= 0.0 || model.is_empty() {
        return;
    }
    let tps = tokens_out as f64 / secs;
    let warn = {
        let mut g = RUN.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = g.as_mut() else { return };
        let k = key(model, r.conc);
        r.local.insert(k.clone(), local);
        let list = r.samples.entry(k.clone()).or_default();
        list.push(tps);
        if !local || list.len() < 2 || r.warned.contains(&k) || !warn_on(core) {
            None
        } else {
            let now = median(list);
            let hist = history_load(&core.data_dir);
            let past = hist.get(&k).cloned().unwrap_or_default();
            let usual = median(&past);
            if past.len() >= MIN_HISTORY && usual > 0.0 && now * SLOW <= usual {
                r.warned.insert(k);
                Some((now, usual))
            } else {
                None
            }
        }
    };
    if let Some((now, usual)) = warn {
        let times = (usual / now).round().max(SLOW) as i64;
        let raz = if (2..=4).contains(&(times % 10)) && !(12..=14).contains(&(times % 100)) { "раза" } else { "раз" };
        let vram = gpu().filter(|g| g.total_mb > 0 && g.used_mb as f64 / g.total_mb as f64 >= 0.95).map(|g| format!(" Видеопамять занята почти полностью ({} из {} ГБ) — скорее всего, модель не поместилась.", gb(g.used_mb), gb(g.total_mb))).unwrap_or_default();
        let text = format!(
            "«{}»: {:.0} ток/с вместо обычных {:.0} — примерно в {} {} медленнее.{} Что проверить в LM Studio: движок CUDA (Ctrl+Shift+R), длину контекста и Parallel.",
            model, now, usual, times, raz, vram
        );
        core.log(&text);
        super::notify::send(core, "speed", "Модель стала заметно медленнее", &text);
    }
}

/// Конец проверки: замеры — в историю «обычной» скорости. Если модель была аномально
/// медленной, они не записываются: иначе «обычной» станет медленная скорость.
pub fn run_finished(core: &Core) {
    let Some(r) = RUN.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if r.samples.is_empty() {
        return;
    }
    let mut hist = history_load(&core.data_dir);
    for (k, v) in r.samples {
        if r.warned.contains(&k) {
            continue;
        }
        let e = hist.entry(k).or_default();
        e.extend(v);
        if e.len() > 40 {
            let cut = e.len() - 40;
            e.drain(..cut);
        }
    }
    history_save(&core.data_dir, &hist);
}

/// Обычная скорость модели (для карточки скорости): медиана прошлых замеров.
pub fn usual_speed(core: &Core, model: &str) -> Option<f64> {
    let conc = core.settings().get("concurrency").i64_or(1).clamp(1, 8) as usize;
    let h = history_load(&core.data_dir);
    let v = h.get(&key(model, conc))?;
    if v.len() >= MIN_HISTORY { Some(median(v)) } else { None }
}

pub struct Gpu {
    pub name: String,
    pub used_mb: u64,
    pub total_mb: u64,
}

/// Видеокарта NVIDIA: имя и занятая память (nvidia-smi ставится вместе с драйвером).
pub fn gpu() -> Option<Gpu> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args(["--query-gpu=name,memory.used,memory.total", "--format=csv,noheader,nounits"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd.spawn().ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() > Duration::from_secs(5) => {
                let _ = child.kill();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(30)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_smi(&out)
}

/// Первая видеокарта из вывода nvidia-smi (с наибольшим объёмом, если их несколько).
pub fn parse_smi(out: &str) -> Option<Gpu> {
    out.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split(',').map(|x| x.trim()).collect();
            if p.len() < 3 {
                return None;
            }
            Some(Gpu { name: p[0].to_string(), used_mb: p[1].parse().ok()?, total_mb: p[2].parse().ok()? })
        })
        .max_by_key(|g| g.total_mb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smi_and_median() {
        let g = parse_smi("NVIDIA GeForce RTX 5080, 15211, 16303\n").unwrap();
        assert_eq!((g.used_mb, g.total_mb), (15211, 16303));
        assert!(g.name.contains("5080"));
        assert!(parse_smi("").is_none());
        assert_eq!(median(&[5.0, 1.0, 3.0]), 3.0);
        assert_eq!(median(&[4.0, 2.0]), 3.0);
    }
}
