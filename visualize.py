#!/usr/bin/env python3
"""
Визуализатор результатов guillotine optimizer.
Запуск: python visualize.py result_task_1.json [--config config.json] [--output схема.png]
"""

import json
import sys
import os
import argparse
import colorsys


def load_json(path):
    with open(path, "r", encoding="utf-8") as f:
        content = f.read().strip()
    if not content:
        print(f"Ошибка: файл пустой: {path}")
        sys.exit(1)
    try:
        return json.loads(content)
    except json.JSONDecodeError as e:
        print(f"Ошибка: файл не является валидным JSON: {path}")
        print(f"  Содержимое файла: {content[:80]!r}")
        print(f"  Детали: {e}")
        sys.exit(1)


def get_page_size(config_path, task_index=0):
    if config_path and os.path.exists(config_path):
        configs = load_json(config_path)
        if isinstance(configs, list) and len(configs) > task_index:
            cfg = configs[task_index]
            return cfg.get("page_width"), cfg.get("page_height")
    return None, None


def generate_color_map(names):
    unique = sorted(set(names))
    colors = {}
    n = len(unique)
    for i, name in enumerate(unique):
        hue = i / max(n, 1)
        r, g, b = colorsys.hsv_to_rgb(hue, 0.50, 0.92)
        colors[name] = (r, g, b, 0.80)
    return colors


def fit_fontsize(text, box_w, box_h, base=9.0, min_size=4.5):
    """Подбирает размер шрифта чтобы текст вписался в прямоугольник (пиксели)."""
    char_w = base * 0.60
    text_w = len(text) * char_w
    text_h = base * 1.2
    scale_w = box_w / max(text_w, 1)
    scale_h = box_h / max(text_h, 1)
    return max(min_size, base * min(scale_w, scale_h, 1.0))


def draw_dim_arrow(ax, x1, y1, x2, y2, label, fontsize=8, color="#333333", vertical=False):
    """Стрелка с подписью размера."""
    import matplotlib.patches as mpatches
    ax.annotate(
        "",
        xy=(x2, y2),
        xytext=(x1, y1),
        arrowprops=dict(arrowstyle="<->", color=color, lw=1.0, mutation_scale=10),
    )
    mx, my = (x1 + x2) / 2, (y1 + y2) / 2
    rotation = 90 if vertical else 0
    ax.text(
        mx, my, label,
        ha="center", va="center",
        fontsize=fontsize, fontweight="bold", color=color, rotation=rotation,
        bbox=dict(boxstyle="round,pad=0.15", facecolor="white", edgecolor="none", alpha=0.9),
        zorder=10,
    )


def visualize(result_path, config_path="config.json", output_path=None, task_index=0):
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
        import matplotlib.patches as patches
        import matplotlib.patheffects as pe
    except ImportError:
        print("Ошибка: matplotlib не установлен.")
        print("Выполните: pip install matplotlib")
        sys.exit(1)

    data = load_json(result_path)
    items = data["items"]
    total_area = data.get("total_area", 1)
    placed_area = data.get("placed_area", 0)
    efficiency = data.get("efficiency", 0.0)
    placed_count = data.get("placed_count", len(items))
    unplaced_count = data.get("unplaced_count", 0)

    page_w, page_h = get_page_size(config_path, task_index)
    if page_w is None:
        page_w = max((it["x"] + it["width"] for it in items), default=1000)
        page_h = max((it["y"] + it["height"] for it in items), default=1000)

    color_map = generate_color_map([it["name"] for it in items])

    # Пропорции фигуры
    ratio = page_h / page_w
    fig_w = 18.0
    fig_h = fig_w * ratio + 3.0

    fig, ax = plt.subplots(figsize=(fig_w, fig_h), dpi=150)
    fig.patch.set_facecolor("#ffffff")
    ax.set_facecolor("#ffffff")

    # Фон листа
    ax.add_patch(patches.Rectangle(
        (0, 0), page_w, page_h,
        linewidth=2, edgecolor="#1a1a1a", facecolor="#f0ede8", zorder=0
    ))

    # Штриховка незаполненной зоны (лёгкая сетка)
    for gx in range(0, page_w, 50):
        ax.axvline(gx, color="#dddddd", lw=0.3, zorder=1)
    for gy in range(0, page_h, 50):
        ax.axhline(gy, color="#dddddd", lw=0.3, zorder=1)

    # Масштаб: сколько пикселей = 1 мм (для выбора размера шрифта)
    # Оценка: fig_w дюймов * 150 dpi / page_w мм
    px_per_mm = (fig_w * 150) / page_w

    # Рисуем детали
    for item in items:
        x, y = item["x"], item["y"]
        w, h = item["width"], item["height"]
        name = item["name"]
        rotated = item.get("rotated", False)
        color = color_map[name]

        rect = patches.FancyBboxPatch(
            (x + 0.5, y + 0.5), w - 1, h - 1,
            boxstyle="square,pad=0",
            linewidth=0.8, edgecolor="#2a2a2a", facecolor=color, zorder=2
        )
        ax.add_patch(rect)

        cx, cy = x + w / 2, y + h / 2

        # Имя детали
        fs_name = fit_fontsize(name, w * px_per_mm * 0.85, h * px_per_mm * 0.40, base=9.0)
        ax.text(
            cx, cy - h * 0.10,
            name,
            ha="center", va="center",
            fontsize=fs_name, fontweight="bold", color="#111111",
            path_effects=[pe.withStroke(linewidth=1.5, foreground="white")],
            zorder=3, clip_on=True
        )

        # Размер детали (WxH)
        dim_label = f"{w}×{h}"
        if rotated:
            dim_label += " ↻"
        fs_dim = fit_fontsize(dim_label, w * px_per_mm * 0.85, h * px_per_mm * 0.28, base=7.5)
        ax.text(
            cx, cy + h * 0.20,
            dim_label,
            ha="center", va="center",
            fontsize=fs_dim, color="#333333",
            path_effects=[pe.withStroke(linewidth=1.2, foreground="white")],
            zorder=3, clip_on=True
        )

        # Площадь детали (только если достаточно места)
        area_label = f"{w * h} мм²"
        fs_area = fit_fontsize(area_label, w * px_per_mm * 0.80, h * px_per_mm * 0.22, base=6.0)
        if fs_area >= 4.5 and w * px_per_mm > 40 and h * px_per_mm > 35:
            ax.text(
                cx, cy + h * 0.42,
                area_label,
                ha="center", va="center",
                fontsize=fs_area, color="#555555",
                path_effects=[pe.withStroke(linewidth=1.0, foreground="white")],
                zorder=3, clip_on=True
            )

    # ── Размерные стрелки листа ──────────────────────────────────────────────
    pad = page_w * 0.045

    # Ширина — снизу листа
    y_arrow = page_h + pad * 0.55
    ax.plot([0, 0], [page_h, y_arrow], color="#444444", lw=0.7, zorder=5)
    ax.plot([page_w, page_w], [page_h, y_arrow], color="#444444", lw=0.7, zorder=5)
    draw_dim_arrow(ax, 0, y_arrow, page_w, y_arrow,
                   f"{page_w} мм", fontsize=10, color="#222222")

    # Высота — справа листа
    x_arrow = page_w + pad * 0.55
    ax.plot([page_w, x_arrow], [0, 0], color="#444444", lw=0.7, zorder=5)
    ax.plot([page_w, x_arrow], [page_h, page_h], color="#444444", lw=0.7, zorder=5)
    draw_dim_arrow(ax, x_arrow, 0, x_arrow, page_h,
                   f"{page_h} мм", fontsize=10, color="#222222", vertical=True)

    # ── Легенда ─────────────────────────────────────────────────────────────
    unique_names = sorted(set(it["name"] for it in items))
    legend_handles = [
        patches.Patch(facecolor=color_map[n], edgecolor="#333333", linewidth=0.7, label=n)
        for n in unique_names
    ]
    legend = ax.legend(
        handles=legend_handles,
        loc="upper left",
        bbox_to_anchor=(1.01, 1.0),
        borderaxespad=0,
        fontsize=8,
        title="Типы деталей",
        title_fontsize=9,
        framealpha=0.95,
        edgecolor="#bbbbbb",
    )

    # ── Заголовок ────────────────────────────────────────────────────────────
    title_lines = [
        f"Раскладка листа  {page_w} × {page_h} мм",
        (f"Деталей размещено: {placed_count} / {placed_count + unplaced_count}   |   "
         f"Площадь: {placed_area:,} / {total_area:,} мм²   |   "
         f"Эффективность: {efficiency:.2f}%"),
    ]
    ax.set_title(
        "\n".join(title_lines),
        fontsize=11, fontweight="bold", pad=12, color="#111111",
        linespacing=1.5,
    )

    # ── Оси ─────────────────────────────────────────────────────────────────
    ax.set_xlim(-pad * 0.3, page_w + pad * 3.5)
    ax.set_ylim(-pad * 0.3, page_h + pad * 2.0)
    ax.set_aspect("equal")
    ax.invert_yaxis()
    ax.axis("off")

    plt.tight_layout()

    if output_path is None:
        base = os.path.splitext(result_path)[0]
        output_path = base + ".png"

    plt.savefig(output_path, dpi=150, bbox_inches="tight", facecolor="white")
    print(f"Схема сохранена: {output_path}")
    plt.close()


def main():
    parser = argparse.ArgumentParser(
        description="Визуализация раскладки guillotine optimizer",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Примеры:\n"
            "  python visualize.py result_task_1.json\n"
            "  python visualize.py result_task_1.json --config config.json\n"
            "  python visualize.py result_task_1.json --output схема.png\n"
            "  python visualize.py result_task_2.json --task 1\n"
        ),
    )
    parser.add_argument("result", help="JSON файл результата (result_task_N.json)")
    parser.add_argument("--config", "-c", default="config.json",
                        help="JSON конфигурации (для размеров листа)")
    parser.add_argument("--output", "-o", default=None,
                        help="Путь для PNG (по умолчанию: <result>.png)")
    parser.add_argument("--task", "-t", type=int, default=0,
                        help="Индекс задачи в config.json (с 0; по умолчанию 0)")
    args = parser.parse_args()

    if not os.path.exists(args.result):
        print(f"Ошибка: файл не найден: {args.result}")
        sys.exit(1)

    visualize(args.result, args.config, args.output, args.task)


if __name__ == "__main__":
    main()
