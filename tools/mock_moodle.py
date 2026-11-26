"""Учебный «Moodle» для проверки прямого подключения (веб-службы REST) без настоящего сайта.

    python tools/mock_moodle.py 8799

Логин: teacher / пароль: secret (ключ: tok123). Курс «Компьютерные сети», группа ИС-21,
задание «Практическая работа 1», тест, оценки и отзывы — у вымышленных студентов.
Сохранённые через mod_assign_save_grade оценки печатаются в консоль и видны в
gradereport_user_get_grade_items.
"""
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

TOKEN = "tok123"
NOW = int(time.time())
USERS = [
    {"id": 11, "fullname": "Тихон Громобоев", "firstname": "Тихон", "lastname": "Громобоев", "email": "t.gromoboev@example.com", "groups": [{"id": 5, "name": "ИС-21"}]},
    {"id": 12, "fullname": "Марфа Лесовикова", "firstname": "Марфа", "lastname": "Лесовикова", "email": "m.lesovikova@example.com", "groups": [{"id": 5, "name": "ИС-21"}]},
    {"id": 13, "fullname": "Савва Медовиков", "firstname": "Савва", "lastname": "Медовиков", "email": "s.medovikov@example.com", "groups": [{"id": 6, "name": "ИС-22"}]},
]
FILES = {
    "/1/assignsubmission_file/submission_files/101/pr1.txt": "Практическая работа 1. Вариант 3.\nIP-адрес 192.168.1.10, маска 255.255.255.0, сеть 192.168.1.0/24.\nВывод: узлы в одной подсети.",
    "/1/assignsubmission_file/submission_files/102/pr1.txt": "Практическая работа 1.\nIP-адрес 10.0.0.5/8. Сеть 10.0.0.0.\nВывод: адрес частный.",
}
SAVED = {}  # (assign, user) -> (grade, feedback)
GRADES = {11: {"pr1": 4.0, "quiz": 3.8, "fb_pr1": "Хорошо, но вывод краткий", "fb_quiz": ""},
          12: {"pr1": 2.7, "quiz": None, "fb_pr1": "Вариант? Похоже на ответ нейросети", "fb_quiz": ""},
          13: {"pr1": None, "quiz": 4.6, "fb_pr1": "", "fb_quiz": ""}}


def fn(name, a):
    if name == "core_webservice_get_site_info":
        return {"sitename": "Учебный Moodle", "fullname": "Преподаватель Пробный", "userid": 2, "release": "4.3"}
    if name == "core_enrol_get_users_courses":
        return [{"id": 7, "fullname": "Компьютерные сети", "shortname": "КС"}]
    if name == "mod_assign_get_assignments":
        return {"courses": [{"id": 7, "assignments": [
            {"id": 1, "cmid": 31, "name": "Практическая работа 1", "duedate": NOW - 86400, "grade": 100, "intro": "Рассчитайте адрес сети"},
            {"id": 2, "cmid": 32, "name": "Практическая работа 2", "duedate": NOW + 7 * 86400, "grade": 5, "intro": ""}]}]}
    if name == "core_enrol_get_enrolled_users":
        return USERS
    if name == "core_group_get_course_groups":
        return [{"id": 5, "name": "ИС-21"}, {"id": 6, "name": "ИС-22"}]
    if name == "mod_assign_get_submissions":
        aid = int(a.get("assignmentids[0]", ["0"])[0])
        subs = []
        if aid == 1:
            for uid, fid in ((11, 101), (12, 102)):
                subs.append({"userid": uid, "status": "submitted", "timemodified": NOW - 3600, "plugins": [
                    {"type": "file", "fileareas": [{"area": "submission_files", "files": [
                        {"filename": "pr1.txt", "fileurl": f"http://127.0.0.1:{PORT}/webservice/pluginfile.php/1/assignsubmission_file/submission_files/{fid}/pr1.txt", "filesize": 100, "timemodified": NOW - 3600}]}]}]})
            subs.append({"userid": 13, "status": "submitted", "timemodified": NOW - 7200, "plugins": [
                {"type": "onlinetext", "editorfields": [{"name": "onlinetext", "text": "<p>Практическая работа 1. Ответ текстом: маска /24.</p>"}]}]})
        return {"assignments": [{"assignmentid": aid, "submissions": subs}], "warnings": []}
    if name == "gradereport_user_get_grade_items":
        out = []
        for u in USERS:
            g = GRADES[u["id"]]
            pr1 = SAVED.get((1, u["id"]), (None, None))[0]
            raw_pr1 = pr1 if pr1 is not None else (g["pr1"] * 20 if g["pr1"] is not None else None)
            out.append({"userid": u["id"], "userfullname": u["fullname"], "gradeitems": [
                {"id": 1, "itemname": "Практическая работа 1", "itemtype": "mod", "itemmodule": "assign", "graderaw": raw_pr1, "grademax": 100, "feedback": SAVED.get((1, u["id"]), (None, None))[1] or g["fb_pr1"]},
                {"id": 2, "itemname": "Тестирование по Разделу 1", "itemtype": "mod", "itemmodule": "quiz", "graderaw": g["quiz"], "grademax": 5, "feedback": g["fb_quiz"]},
                {"id": 3, "itemname": None, "itemtype": "course", "itemmodule": None, "graderaw": 3.5, "grademax": 5, "feedback": ""}]})
        return {"usergrades": out, "warnings": []}
    if name == "mod_assign_save_grade":
        key = (int(a["assignmentid"][0]), int(a["userid"][0]))
        SAVED[key] = (float(a["grade"][0]), a.get("plugindata[assignfeedbackcomments_editor][text]", [""])[0])
        print("SAVED", key, SAVED[key], flush=True)
        return None
    return {"exception": "invalid_parameter_exception", "errorcode": "invalidrecord", "message": "Нет такой функции: " + name}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, code, body, ctype="application/json; charset=utf-8"):
        b = body if isinstance(body, bytes) else body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path.startswith("/webservice/pluginfile.php"):
            if q.get("token", [""])[0] != TOKEN:
                return self.send(403, json.dumps({"error": "invalid token", "errorcode": "invalidtoken"}))
            rest = u.path[len("/webservice/pluginfile.php"):]
            if rest in FILES:
                return self.send(200, FILES[rest], "text/plain; charset=utf-8")
            return self.send(404, "нет файла")
        self.send(404, "нет")

    def do_POST(self):
        u = urlparse(self.path)
        n = int(self.headers.get("Content-Length", 0))
        a = parse_qs(self.rfile.read(n).decode("utf-8"), keep_blank_values=True)
        if u.path == "/login/token.php":
            if a.get("username", [""])[0] == "teacher" and a.get("password", [""])[0] == "secret":
                return self.send(200, json.dumps({"token": TOKEN}))
            return self.send(200, json.dumps({"error": "Неверный логин или пароль", "errorcode": "invalidlogin"}, ensure_ascii=False))
        if u.path == "/webservice/rest/server.php":
            if a.get("wstoken", [""])[0] != TOKEN:
                return self.send(200, json.dumps({"exception": "moodle_exception", "errorcode": "invalidtoken", "message": "Неверный ключ"}, ensure_ascii=False))
            r = fn(a.get("wsfunction", [""])[0], a)
            return self.send(200, json.dumps(r, ensure_ascii=False))
        self.send(404, "нет")


if __name__ == "__main__":
    PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8799
    ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
