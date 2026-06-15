#!/usr/bin/env python3
"""
Flask-модуль для guillotine optimizer.

Веб-интерфейс (порт 8001):
  - поле для размера листа (ширина × высота, мм)
  - таблица для ввода изделий: ширина, высота, количество (мм)
  - кнопка "Запустить" → запускает Rust-оптимизатор → строит схему раскладки

Результат на странице:
  - картинка раскладки (PNG)
  - количество размещённых деталей
  - список нераскроенных деталей (по типам)

Запуск:
    python3 webapp/app.py
Открыть: http://localhost:8001
"""

import json
import os
import subprocess
import sys
import time
import uuid

from flask import Flask, render_template, request, send_from_directory

# ── Пути ─────────────────────────────────────────────────────────────────────
WEBAPP_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.dirname(WEBAPP_DIR)
RUNS_DIR = os.path.join(WEBAPP_DIR, "runs")
VISUALIZE_PY = os.path.join(PROJECT_ROOT, "visualize.py")

# Бинарник оптимизатора (release; fallback на debug)
BIN_RELEASE = os.path.join(PROJECT_ROOT, "target", "release", "guillotine_optimizer")
BIN_DEBUG = os.path.join(PROJECT_ROOT, "target", "debug", "guillotine_optimizer")

os.makedirs(RUNS_DIR, exist_ok=True)

# Импортируем визуализатор напрямую (без вызова отдельного процесса)
sys.path.insert(0, PROJECT_ROOT)
import visualize  # noqa: E402

app = Flask(__name__)


def find_binary():
    if os.path.exists(BIN_RELEASE):
        return BIN_RELEASE
    if os.path.exists(BIN_DEBUG):
        return BIN_DEBUG
    return None


def run_optimizer(page_w, page_h, elements, timeout_s, goal_driven_s):
    """
    Запускает Rust-оптимизатор в отдельной рабочей папке.
    Возвращает (result_dict, png_filename, error_str).
    """
    binary = find_binary()
    if binary is None:
        return None, None, (
            "Бинарник не найден. Соберите его: cargo build --release"
        )

    run_id = uuid.uuid4().hex[:12]
    work_dir = os.path.join(RUNS_DIR, run_id)
    os.makedirs(work_dir, exist_ok=True)

    config = [{
        "page_width": page_w,
        "page_height": page_h,
        "max_depth": 60,
        "elements": elements,
    }]
    config_path = os.path.join(work_dir, "config.json")
    with open(config_path, "w", encoding="utf-8") as f:
        json.dump(config, f, ensure_ascii=False, indent=2)

    # Оптимизатор пишет result_task_1.json в текущую папку (cwd=work_dir).
    # Результат сохраняется инкрементально, поэтому даже при жёстком таймауте
    # subprocess мы читаем лучшее найденное на данный момент.
    cmd = [binary, "-f", config_path, "-t", str(timeout_s),
           "-g", str(goal_driven_s)]
    # запас на пост-обработку (goal-driven фаза + прочее) с небольшим буфером
    hard_timeout = timeout_s + goal_driven_s + 20
    try:
        subprocess.run(
            cmd, cwd=work_dir,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            timeout=hard_timeout,
        )
    except subprocess.TimeoutExpired:
        pass  # используем то, что успело сохраниться
    except Exception as e:
        return None, None, f"Ошибка запуска оптимизатора: {e}"

    result_path = os.path.join(work_dir, "result_task_1.json")
    if not os.path.exists(result_path):
        return None, None, "Оптимизатор не вернул результат (нет result_task_1.json)."

    # Файл мог быть только что переписан оптимизатором — читаем с повтором,
    # чтобы не наткнуться на недописанный JSON.
    result = None
    for attempt in range(5):
        try:
            with open(result_path, "r", encoding="utf-8") as f:
                result = json.load(f)
            break
        except (json.JSONDecodeError, ValueError):
            time.sleep(0.2)
    if result is None:
        return None, None, "Результат повреждён (не удалось прочитать JSON)."

    # Генерируем картинку
    png_name = f"{run_id}.png"
    png_path = os.path.join(RUNS_DIR, png_name)
    try:
        visualize.visualize(result_path, config_path, png_path, 0)
    except Exception as e:
        return result, None, f"Раскладка получена, но визуализация упала: {e}"

    return result, png_name, None


def compute_unplaced(elements, items):
    """Считает нераскроенные детали по типам: запрошено − размещено."""
    placed = {}
    for it in items:
        placed[it["name"]] = placed.get(it["name"], 0) + 1
    unplaced = []
    for el in elements:
        name = el["name"]
        left = el["count"] - placed.get(name, 0)
        if left > 0:
            unplaced.append({
                "name": name,
                "width": el["width"],
                "height": el["height"],
                "count": left,
            })
    return unplaced


@app.route("/", methods=["GET"])
def index():
    return render_template("index.html")


@app.errorhandler(Exception)
def handle_any_error(e):
    """Любая необработанная ошибка отдаётся как JSON, а не HTML-страница."""
    from werkzeug.exceptions import HTTPException
    if isinstance(e, HTTPException):
        return e  # штатные ответы (404 и т.п.) не трогаем
    import traceback
    traceback.print_exc()
    return {"error": f"Внутренняя ошибка сервера: {e}"}, 500


@app.route("/optimize", methods=["POST"])
def optimize():
    data = request.get_json(force=True, silent=True)
    if data is None:
        return {"error": "Тело запроса не является JSON."}, 400
    try:
        page_w = int(data["page_width"])
        page_h = int(data["page_height"])
        timeout_s = max(1, min(int(data.get("timeout", 15)), 300))
        goal_driven_s = max(0, min(int(data.get("goal_driven", 0)), 300))
        raw_parts = data["parts"]
    except (KeyError, ValueError, TypeError):
        return {"error": "Некорректные входные данные."}, 400

    elements = []
    for i, p in enumerate(raw_parts):
        try:
            w = int(p["width"])
            h = int(p["height"])
            c = int(p["count"])
        except (KeyError, ValueError, TypeError):
            continue
        if w <= 0 or h <= 0 or c <= 0:
            continue
        elements.append({
            "width": w, "height": h, "count": c,
            "name": p.get("name") or f"{w}x{h}",
        })

    if page_w <= 0 or page_h <= 0:
        return {"error": "Размер листа должен быть положительным."}, 400
    if not elements:
        return {"error": "Добавьте хотя бы одно изделие."}, 400

    t0 = time.time()
    result, png_name, error = run_optimizer(
        page_w, page_h, elements, timeout_s, goal_driven_s)
    elapsed = round(time.time() - t0, 1)

    if result is None:
        return {"error": error}, 500

    items = result.get("items", [])
    unplaced = compute_unplaced(elements, items)
    total_requested = sum(el["count"] for el in elements)

    return {
        "image": f"/runs/{png_name}" if png_name else None,
        "placed_count": result.get("placed_count", len(items)),
        "total_requested": total_requested,
        "unplaced_total": sum(u["count"] for u in unplaced),
        "unplaced": unplaced,
        "efficiency": round(result.get("efficiency", 0.0), 2),
        "elapsed": elapsed,
        "warning": error,  # на случай частичной ошибки (напр. визуализации)
    }


@app.route("/runs/<path:filename>")
def serve_run(filename):
    return send_from_directory(RUNS_DIR, filename)


if __name__ == "__main__":
    print("=" * 60)
    print("  Guillotine Optimizer — веб-интерфейс")
    print("  Открой: http://localhost:8001")
    print("=" * 60)
    app.run(host="0.0.0.0", port=8001, debug=True)
