// Проверка интерфейса без Windows: devserver + Chromium (Playwright).
// Открывает все шаги, журнал, настройки, окна Moodle и архива и падает,
// если в странице случилась ошибка JavaScript или что-то не отрисовалось.
//
//   node tools/ui_smoke.js http://127.0.0.1:8790 [папка-для-скриншотов]
//
// Данные — только вымышленные студенты (создаются через API).
const { chromium } = require("playwright");

const base = process.argv[2] || "http://127.0.0.1:8790";
const shots = process.argv[3] || "";

async function api(method, payload = {}) {
  const r = await fetch(`${base}/api/${method}`, { method: "POST", body: JSON.stringify(payload) });
  const j = await r.json();
  if (j && j.error) throw new Error(`${method}: ${j.error}`);
  return j;
}

(async () => {
  // журнал вымышленной группы: список, темы, оценки (в том числе дробные), свой столбец
  const g = "ИС-99";
  const names = ["Громобоев Тихон", "Лесовикова Марфа", "Медовиков Савва", "Пряничкова Ульяна"];
  await api("journal_roster", { group: g, names });
  await api("journal_topic", { group: g, code: "ПР1", action: "set", topic: { kind: "work", title: "Практическая работа 1", issued: "2026-09-10" } });
  await api("journal_topic", { group: g, code: "Тестирование по Разделу 1. Системы", action: "set", topic: { kind: "test", issued: "2026-09-20" } });
  await api("journal_topic", { group: g, code: "Пропущено пар", action: "set", topic: { kind: "custom", unit: "пар" } });
  const grades = [5, 4.5, 2.7, 3];
  for (let i = 0; i < names.length; i++) {
    await api("journal_set", { group: g, student: names[i], code: "ПР1", grade: grades[i], comment: i === 1 ? "Вариант?" : "" });
    await api("journal_set", { group: g, student: names[i], code: "Тестирование по Разделу 1. Системы", grade: 2 + i * 0.85 });
  }

  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => { if (m.type() === "error" && !/favicon|Failed to load resource/.test(m.text())) errors.push(m.text()); });
  await page.goto(base);
  await page.waitForTimeout(1200);
  const shot = async (name) => { if (shots) await page.screenshot({ path: `${shots}/${name}.png` }); };

  for (const v of ["tasks", "works", "model", "check", "report", "mcplog", "journal"]) {
    await page.click(`[data-view="${v}"]`);
    await page.waitForTimeout(700);
    const empty = await page.$eval("#view", (el) => el.innerText.trim().length === 0);
    if (empty) errors.push(`пустой экран: ${v}`);
    await shot(`view-${v}`);
  }
  // журнал: группа, клетки, шапка, значок «в?»
  await page.click(`[data-g="${g}"]`).catch(() => {});
  await page.waitForTimeout(600);
  const cells = await page.$$eval(".jt td.cell .gm", (x) => x.map((e) => e.textContent));
  if (!cells.includes("2,7")) errors.push(`в журнале нет дробной оценки 2,7: ${cells.join(" ")}`);
  if (!(await page.$(".jt td.cell .nv"))) errors.push("нет значка «в?» у отзыва «Вариант?»");
  if (!(await page.$$eval(".th-code", (x) => x.some((e) => e.textContent === "Тест Р1")))) errors.push("длинное название теста не сократилось до «Тест Р1»");
  await shot("journal");
  // окна: Moodle ⇄ журнал, должники, тема
  for (const [btn, name] of [["#jMoodle", "moodle"], ["#jTopics", "topics"]]) {
    const b = await page.$(btn);
    if (!b) continue;
    await b.click();
    await page.waitForTimeout(600);
    await shot(name);
    await page.keyboard.press("Escape");
    await page.locator("#mjClose").click({ timeout: 1000 }).catch(() => {});
    await page.waitForTimeout(400);
  }
  // настройки (Moodle напрямую, резервные копии, переносной режим)
  await page.click("#setBtn");
  await page.waitForTimeout(800);
  if (!(await page.$("#sMws"))) errors.push("в настройках нет раздела «Moodle напрямую»");
  await shot("settings");
  await page.keyboard.press("Escape");
  await page.waitForTimeout(400);
  // архив прошлых лет
  await page.click('[data-view="works"]');
  await page.waitForTimeout(500);
  await page.click("#arcBtn");
  await page.waitForTimeout(600);
  await shot("archive");

  await browser.close();
  if (errors.length) {
    console.error("Ошибки интерфейса:\n - " + errors.join("\n - "));
    process.exit(1);
  }
  console.log("Интерфейс: всё открылось без ошибок");
})().catch((e) => { console.error(e); process.exit(1); });
