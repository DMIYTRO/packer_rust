use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use serde::{Deserialize, Serialize};
use clap::Parser;
use std::time::{Duration, Instant};
use rayon::prelude::*;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, AtomicBool, AtomicU32, Ordering};
use std::io::Write;

// ============================================================================
// СТРУКТУРЫ ДАННЫХ
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ElementType {
    width: u32,
    height: u32,
    count: u32,
    name: String,
}

#[derive(Debug, Clone, Serialize)]
struct PlacedItem {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    name: String,
    rotated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MemoKey {
    width: u32,
    height: u32,
    counts_hash: u64,
}

impl MemoKey {
    fn new(width: u32, height: u32, counts: &[u32]) -> Self {
        let mut hasher = DefaultHasher::new();
        counts.hash(&mut hasher);
        Self {
            width,
            height,
            counts_hash: hasher.finish(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Solution {
    items: Vec<PlacedItem>,
    remaining_counts: Vec<u32>,
    total_area: u64,
}

type MemoCache = HashMap<MemoKey, Solution>;

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    page_width: u32,
    page_height: u32,
    elements: Vec<ElementType>,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Parallel Ultra-Efficient Guillotine Optimizer")]
struct Args {
    #[arg(short = 'f', long)]
    file: String,
    #[arg(short = 't', long, default_value = "1000")]
    timeout_seconds: u64,
    #[arg(short = 'b', long, default_value = "12")]
    beam_width: usize,
}

// Глобальные переменные мониторинга
static GLOBAL_BEST_AREA: AtomicU64 = AtomicU64::new(0);
static GLOBAL_BEST_COUNT: AtomicU32 = AtomicU32::new(0);
static FOUND_PERFECT: AtomicBool = AtomicBool::new(false);
static BRANCHES_PROCESSED: AtomicU32 = AtomicU32::new(0);
static TOTAL_ITERATIONS: AtomicU64 = AtomicU64::new(0);
static TIMEOUT_REACHED: AtomicBool = AtomicBool::new(false);

// ============================================================================
// ЭВРИСТИКА
// ============================================================================

fn score_move(
    container_w: u32,
    container_h: u32,
    item_w: u32,
    item_h: u32,
    count_left: u32,
    all_specs: &[ElementType],
    all_counts: &[u32]
) -> i64 {
    let mut score: i64 = item_w as i64 * item_h as i64;

    let exact_w = item_w == container_w;
    let exact_h = item_h == container_h;

    // Максимальный приоритет для точного заполнения
    if exact_w && exact_h { return i64::MAX / 4; }
    if exact_w { score += 100_000_000; }  // Увеличено с 80M
    if exact_h { score += 100_000_000; }

    let rem_w = container_w.saturating_sub(item_w);
    let rem_h = container_h.saturating_sub(item_h);

    // Минимизация отходов - более агрессивный штраф
    let waste_area = (rem_w as i64 * container_h as i64) + (container_w as i64 * rem_h as i64) - (rem_w as i64 * rem_h as i64);
    score -= waste_area / 5;  // Увеличен штраф вдвое

    let mut can_fit_any = false;
    let mut exact_fit_count = 0;

    for (i, spec) in all_specs.iter().enumerate() {
        if all_counts[i] > 0 {
            // Проверка возможности размещения в остатках
            if (spec.width <= rem_w && spec.height <= container_h) || (spec.height <= rem_w && spec.width <= container_h) ||
                (spec.width <= container_w && spec.height <= rem_h) || (spec.height <= container_w && spec.width <= rem_h) {
                can_fit_any = true;

                // Огромный бонус за точное совпадение с остатками
                if spec.width == rem_w || spec.height == rem_w {
                    score += 25_000_000;  // Увеличено с 15M
                    exact_fit_count += 1;
                }
                if spec.width == rem_h || spec.height == rem_h {
                    score += 25_000_000;
                    exact_fit_count += 1;
                }

                // Бонус за возможность разместить маленькие детали
                if spec.width * spec.height < 4000 {
                    score += 8_000_000;
                }
            }
        }
    }

    // КРИТИЧЕСКИЙ штраф за бесполезные остатки
    if !can_fit_any && (rem_w > 0 || rem_h > 0) {
        let waste = (rem_w * container_h + container_w * rem_h) as i64;
        score -= 300_000_000 + waste * 1000;  // Прогрессивный штраф
    }

    // Бонус за наличие деталей для продолжения
    score += count_left as i64 * 1500;  // Увеличено с 1000

    // Огромный бонус за несколько точных совпадений
    score += exact_fit_count * 10_000_000;  // Увеличено с 5M

    score
}

// ============================================================================
// ПОИСК
// ============================================================================

fn solve_recursive(
    width: u32,
    height: u32,
    items_counts: &[u32],
    item_specs: &[ElementType],
    memo: &mut MemoCache,
    depth: u32,
    start_time: &Instant,
    timeout: &Duration,
    beam_width: usize,
) -> Option<Solution> {
    TOTAL_ITERATIONS.fetch_add(1, Ordering::Relaxed);

    // Проверяем глобальный флаг таймаута
    if FOUND_PERFECT.load(Ordering::Relaxed) || TIMEOUT_REACHED.load(Ordering::Relaxed) {
        return None;
    }

    // Периодически проверяем таймаут
    if depth % 16 == 0 && start_time.elapsed() > *timeout {
        TIMEOUT_REACHED.store(true, Ordering::Relaxed);
        return None;
    }

    let memo_key = if depth > 1 {
        let key = MemoKey::new(width, height, items_counts);
        if let Some(cached) = memo.get(&key) {
            return Some(cached.clone());
        }
        Some(key)
    } else {
        None
    };

    let mut best_solution = Solution {
        items: Vec::new(),
        remaining_counts: items_counts.to_vec(),
        total_area: 0,
    };

    let mut candidates = Vec::new();
    // Подсчитываем оставшиеся детали
    let total_remaining: u32 = items_counts.iter().sum();
    let is_final_stage = total_remaining <= 5;  // Последние 5 деталей

    for (i, spec) in item_specs.iter().enumerate() {
        if items_counts[i] == 0 { continue; }

        let mut score_normal = score_move(width, height, spec.width, spec.height, items_counts[i], item_specs, items_counts);
        let mut score_rotated = score_move(width, height, spec.height, spec.width, items_counts[i], item_specs, items_counts);

        // На финальной стадии приоритет маленьким деталям
        if is_final_stage {
            let item_area = spec.width * spec.height;
            if item_area < 4000 {
                score_normal += 50_000_000;
                score_rotated += 50_000_000;
            }
        }

        if spec.width <= width && spec.height <= height {
            candidates.push((i, spec.width, spec.height, false, score_normal));
        }
        if spec.width != spec.height && spec.height <= width && spec.width <= height {
            candidates.push((i, spec.height, spec.width, true, score_rotated));
        }
    }

    if candidates.is_empty() { return Some(best_solution); }
    candidates.sort_by(|a, b| b.4.cmp(&a.4));

    // Адаптивная ширина луча: более агрессивная на средних глубинах
    let current_beam = if depth < 3 {
        beam_width
    } else if depth < 7 {
        (beam_width * 3 / 4).max(8)
    } else if depth < 12 {
        5
    } else if depth < 18 {
        4
    } else {
        3
    };

    for (idx, item_w, item_h, rotated, _) in candidates.into_iter().take(current_beam) {
        let mut new_counts = items_counts.to_vec();
        new_counts[idx] -= 1;
        let item_area = item_w as u64 * item_h as u64;

        let splits = if item_w == width { vec![true] } else if item_h == height { vec![false] } else { vec![true, false] };

        for split_vertical in splits {
            let res = if split_vertical {
                solve_recursive(width - item_w, item_h, &new_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width).and_then(|res_r| {
                    solve_recursive(width, height - item_h, &res_r.remaining_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width).map(|res_b| {
                        let total = item_area + res_r.total_area + res_b.total_area;
                        let mut items = vec![PlacedItem { x: 0, y: 0, width: item_w, height: item_h, name: item_specs[idx].name.clone(), rotated }];
                        for mut p in res_r.items { p.x += item_w; items.push(p); }
                        for mut p in res_b.items { p.y += item_h; items.push(p); }
                        Solution { items, remaining_counts: res_b.remaining_counts, total_area: total }
                    })
                })
            } else {
                solve_recursive(item_w, height - item_h, &new_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width).and_then(|res_b| {
                    solve_recursive(width - item_w, height, &res_b.remaining_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width).map(|res_r| {
                        let total = item_area + res_b.total_area + res_r.total_area;
                        let mut items = vec![PlacedItem { x: 0, y: 0, width: item_w, height: item_h, name: item_specs[idx].name.clone(), rotated }];
                        for mut p in res_b.items { p.y += item_h; items.push(p); }
                        for mut p in res_r.items { p.x += item_w; items.push(p); }
                        Solution { items, remaining_counts: res_r.remaining_counts, total_area: total }
                    })
                })
            };

            if let Some(sol) = res {
                if sol.total_area > best_solution.total_area {
                    best_solution = sol;
                    let c_best = GLOBAL_BEST_AREA.load(Ordering::Relaxed);
                    if best_solution.total_area > c_best {
                        GLOBAL_BEST_AREA.store(best_solution.total_area, Ordering::Relaxed);
                        GLOBAL_BEST_COUNT.store(best_solution.items.len() as u32, Ordering::Relaxed);
                    }
                    if best_solution.total_area == (width as u64 * height as u64) { return Some(best_solution); }
                }
            }
        }
    }

    if let Some(key) = memo_key { memo.insert(key, best_solution.clone()); }
    Some(best_solution)
}

// Попытка заполнить оставшиеся промежутки мелкими деталями
fn try_fill_gaps(
    page_width: u32,
    page_height: u32,
    placed: &[PlacedItem],
    specs: &[ElementType],
    remaining: &[u32],
) -> Option<Vec<PlacedItem>> {
    let mut additional_items = Vec::new();
    let mut new_remaining = remaining.to_vec();

    // Сортируем оставшиеся детали от меньшей к большей
    let mut remaining_indices: Vec<(usize, u32)> = remaining.iter()
        .enumerate()
        .filter(|(_, &count)| count > 0)
        .map(|(i, &count)| (i, specs[i].width * specs[i].height))
        .collect();
    remaining_indices.sort_by_key(|&(_, area)| area);

    // Для каждой оставшейся детали ищем свободное место
    for (i, _) in remaining_indices {
        if new_remaining[i] == 0 { continue; }
        let spec = &specs[i];

        // Пробуем оба варианта ориентации
        for &(w, h, rot) in &[(spec.width, spec.height, false), (spec.height, spec.width, true)] {
            let mut found = false;

            // Умный поиск: сначала вдоль границ, затем с большим шагом
            let step = w.min(h).max(5); // Адаптивный шаг

            'search: for y in (0..=page_height.saturating_sub(h)).step_by(step as usize) {
                for x in (0..=page_width.saturating_sub(w)).step_by(step as usize) {
                    // Проверяем пересечения
                    let mut overlaps = false;
                    for item in placed.iter().chain(additional_items.iter()) {
                        if x < item.x + item.width && x + w > item.x &&
                           y < item.y + item.height && y + h > item.y {
                            overlaps = true;
                            break;
                        }
                    }

                    if !overlaps {
                        additional_items.push(PlacedItem {
                            x, y, width: w, height: h,
                            name: spec.name.clone(),
                            rotated: rot,
                        });
                        new_remaining[i] -= 1;
                        found = true;
                        break 'search;
                    }
                }
            }

            if found { break; }
        }
    }

    if !additional_items.is_empty() {
        Some(additional_items)
    } else {
        None
    }
}

fn save_result(task_id: usize, sol: &Solution, target_area: u64) {
    let efficiency = (sol.total_area as f64 / target_area as f64) * 100.0;
    let out_file = format!("result_task_{}.json", task_id);
    let output = serde_json::json!({
        "efficiency": efficiency,
        "total_area": target_area,
        "placed_area": sol.total_area,
        "placed_count": sol.items.len(),
        "items": sol.items,
        "unplaced_count": sol.remaining_counts.iter().sum::<u32>()
    });
    // Прямая запись с немедленным закрытием файла
    if let Ok(mut file) = std::fs::File::create(&out_file) {
        let _ = serde_json::to_writer_pretty(&mut file, &output);
        let _ = file.sync_all(); // Гарантируем запись на диск
    }
}

fn main() {
    let args = Args::parse();
    let content = std::fs::read_to_string(&args.file).expect("Ошибка чтения файла");
    let configs: Vec<Config> = serde_json::from_str(&content).expect("Ошибка парсинга JSON");

    let start = Instant::now();
    let timeout = Duration::from_secs(args.timeout_seconds);
    let timeout_secs = args.timeout_seconds;

    // Фоновый поток мониторинга
    let monitor_handle = {
        let start_clone = start.clone();
        std::thread::spawn(move || {
            let mut last_iters = 0;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let total_area = GLOBAL_BEST_AREA.load(Ordering::Relaxed);
                let best_count = GLOBAL_BEST_COUNT.load(Ordering::Relaxed);
                let iters = TOTAL_ITERATIONS.load(Ordering::Relaxed);
                let speed = (iters - last_iters) * 2; // *2 т.к. проверяем каждые 500мс
                last_iters = iters;
                let elapsed = start_clone.elapsed().as_secs();

                if total_area > 0 {
                    print!(
                        "\r   [МОНИТОРИНГ] Время: {}/{} сек | Итераций: {} млн | Скорость: {} ит/сек | Лучшее: {} мм² (Деталей: {})   ",
                        elapsed,
                        timeout_secs,
                        iters / 1_000_000,
                        speed,
                        total_area,
                        best_count
                    );
                    std::io::stdout().flush().ok();
                }

                // Проверяем условия выхода
                if FOUND_PERFECT.load(Ordering::Relaxed) || TIMEOUT_REACHED.load(Ordering::Relaxed) {
                    println!();
                    break;
                }

                // Устанавливаем флаг таймаута
                if elapsed >= timeout_secs {
                    TIMEOUT_REACHED.store(true, Ordering::Relaxed);
                    println!("\n   ⏱️ Таймаут {} сек достигнут!", timeout_secs);
                    break;
                }
            }
        })
    };

    for (task_i, cfg) in configs.iter().enumerate() {
        // Проверяем таймаут перед началом задачи
        if TIMEOUT_REACHED.load(Ordering::Relaxed) {
            println!("\n⏱️ Таймаут достигнут. Пропуск оставшихся задач.");
            break;
        }

        let task_id = task_i + 1;
        let total_items_to_place: u32 = cfg.elements.iter().map(|e| e.count).sum();
        println!("\n🚀 ЗАДАЧА #{} [{}x{}]", task_id, cfg.page_width, cfg.page_height);
        println!("   Цель: разместить {} деталей.", total_items_to_place);

        let mut specs = cfg.elements.clone();

        // НОВАЯ СТРАТЕГИЯ: сортируем по сложности размещения
        // Приоритет: большие детали, затем нестандартные пропорции
        specs.sort_by(|a, b| {
            let area_a = a.width * a.height;
            let area_b = b.width * b.height;
            let aspect_a = (a.width as f32 / a.height as f32).max(a.height as f32 / a.width as f32);
            let aspect_b = (b.width as f32 / b.height as f32).max(b.height as f32 / b.width as f32);

            // Сначала по площади, потом по "странности" пропорций
            match area_b.cmp(&area_a) {
                std::cmp::Ordering::Equal => aspect_b.partial_cmp(&aspect_a).unwrap_or(std::cmp::Ordering::Equal),
                other => other,
            }
        });

        let counts: Vec<u32> = specs.iter().map(|s| s.count).collect();
        let target_area = cfg.page_width as u64 * cfg.page_height as u64;

        GLOBAL_BEST_AREA.store(0, Ordering::SeqCst);
        GLOBAL_BEST_COUNT.store(0, Ordering::SeqCst);
        FOUND_PERFECT.store(false, Ordering::SeqCst);
        BRANCHES_PROCESSED.store(0, Ordering::SeqCst);
        TOTAL_ITERATIONS.store(0, Ordering::SeqCst);
        // НЕ сбрасываем TIMEOUT_REACHED - он должен сохраняться между задачами

        let mut first_moves = Vec::new();
        for (i, spec) in specs.iter().enumerate() {
            if counts[i] > 0 {
                if spec.width <= cfg.page_width && spec.height <= cfg.page_height {
                    let s = score_move(cfg.page_width, cfg.page_height, spec.width, spec.height, counts[i], &specs, &counts);
                    first_moves.push((i, spec.width, spec.height, false, s));
                }
                if spec.width != spec.height && spec.height <= cfg.page_width && spec.width <= cfg.page_height {
                    let s = score_move(cfg.page_width, cfg.page_height, spec.height, spec.width, counts[i], &specs, &counts);
                    first_moves.push((i, spec.height, spec.width, true, s));
                }
            }
        }

        first_moves.sort_by(|a, b| b.4.cmp(&a.4));

        // Расширенный поиск: обрабатываем ВСЕ возможные первые ходы
        // Не ограничиваем количество веток для тщательного исследования
        println!("   [ИНФО] Обработка {} веток (полный перебор первого уровня)...", first_moves.len());

        let best_global_sol = Arc::new(Mutex::new(Solution { items: vec![], remaining_counts: counts.clone(), total_area: 0 }));
        let total_b = first_moves.len() as u32;

        first_moves.into_par_iter().for_each(|(idx, item_w, item_h, rotated, _)| {
            // Проверяем флаги перед началом обработки ветки
            if FOUND_PERFECT.load(Ordering::Relaxed) || TIMEOUT_REACHED.load(Ordering::Relaxed) {
                return;
            }

            let mut local_counts = counts.clone();
            local_counts[idx] -= 1;
            let mut local_memo = HashMap::new();

            for split_v in [true, false] {
                // Проверяем флаги перед каждым split
                if FOUND_PERFECT.load(Ordering::Relaxed) || TIMEOUT_REACHED.load(Ordering::Relaxed) {
                    break;
                }

                let res = if split_v {
                    solve_recursive(cfg.page_width - item_w, item_h, &local_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width).and_then(|res_r| {
                        solve_recursive(cfg.page_width, cfg.page_height - item_h, &res_r.remaining_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width).map(|res_b| {
                            let mut items = vec![PlacedItem { x: 0, y: 0, width: item_w, height: item_h, name: specs[idx].name.clone(), rotated }];
                            for mut p in res_r.items { p.x += item_w; items.push(p); }
                            for mut p in res_b.items { p.y += item_h; items.push(p); }
                            Solution { items, remaining_counts: res_b.remaining_counts, total_area: (item_w * item_h) as u64 + res_r.total_area + res_b.total_area }
                        })
                    })
                } else {
                    solve_recursive(item_w, cfg.page_height - item_h, &local_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width).and_then(|res_b| {
                        solve_recursive(cfg.page_width - item_w, cfg.page_height, &res_b.remaining_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width).map(|res_r| {
                            let mut items = vec![PlacedItem { x: 0, y: 0, width: item_w, height: item_h, name: specs[idx].name.clone(), rotated }];
                            for mut p in res_b.items { p.y += item_h; items.push(p); }
                            for mut p in res_r.items { p.x += item_w; items.push(p); }
                            Solution { items, remaining_counts: res_r.remaining_counts, total_area: (item_w * item_h) as u64 + res_b.total_area + res_r.total_area }
                        })
                    })
                };

                if let Some(sol) = res {
                    let mut is_new_record = false;
                    {
                        let mut best_g = best_global_sol.lock().unwrap();
                        if sol.total_area > best_g.total_area {
                            *best_g = sol.clone();
                            is_new_record = true;
                        }
                    }

                    if is_new_record {
                        let eff = (sol.total_area as f64 / target_area as f64) * 100.0;
                        println!(
                            "\n   [РЕКОРД] Заполнение: {:.4}% | Площадь: {} мм² | Деталей: {}/{}",
                            eff, sol.total_area, sol.items.len(), total_items_to_place
                        );

                        // Сохраняем немедленно
                        save_result(task_id, &sol, target_area);

                        if sol.total_area == target_area {
                            FOUND_PERFECT.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            BRANCHES_PROCESSED.fetch_add(1, Ordering::Relaxed);
        });

        let mut final_sol = best_global_sol.lock().unwrap().clone();

        // ПОСТ-ОБРАБОТКА: пытаемся заполнить оставшиеся промежутки
        let unplaced: u32 = final_sol.remaining_counts.iter().sum();
        if unplaced > 0 {
            println!("\n   [ПОСТ-ОБРАБОТКА] Попытка заполнить промежутки для {} деталей...", unplaced);
            if let Some(additional) = try_fill_gaps(cfg.page_width, cfg.page_height, &final_sol.items, &specs, &final_sol.remaining_counts) {
                let added_count = additional.len();
                let added_area: u64 = additional.iter().map(|p| p.width as u64 * p.height as u64).sum();

                // Обновляем счетчики
                for item in &additional {
                    for (i, spec) in specs.iter().enumerate() {
                        if spec.name == item.name {
                            final_sol.remaining_counts[i] -= 1;
                            break;
                        }
                    }
                }

                final_sol.items.extend(additional);
                final_sol.total_area += added_area;

                println!("   [УСПЕХ] Добавлено {} деталей (+{} мм²)", added_count, added_area);
            }
        }

        let final_eff = (final_sol.total_area as f64 / target_area as f64) * 100.0;
        println!("\n✅ ЗАДАЧА #{} ЗАВЕРШЕНА. Итог: {:.4}%", task_id, final_eff);

        // ВСЕГДА сохраняем финальный результат
        save_result(task_id, &final_sol, target_area);
        println!("   📁 Результат сохранён в result_task_{}.json", task_id);

        // Автоматически запускаем визуализацию
        let result_file = format!("result_task_{}.json", task_id);
        let task_idx = (task_id - 1).to_string();
        let vis_status = std::process::Command::new("python3")
            .args(["visualize.py", &result_file, "--config", &args.file, "--task", &task_idx])
            .status();
        match vis_status {
            Ok(s) if s.success() => println!("   🖼️  Схема сохранена в result_task_{}.png", task_id),
            Ok(_) => eprintln!("   ⚠️  visualize.py завершился с ошибкой"),
            Err(e) => eprintln!("   ⚠️  Не удалось запустить python3: {}", e),
        }
    }

    // Принудительно останавливаем поток мониторинга
    TIMEOUT_REACHED.store(true, Ordering::SeqCst);
    FOUND_PERFECT.store(true, Ordering::SeqCst);

    // Ждем завершения потока с таймаутом
    let join_timeout = std::time::Duration::from_secs(2);
    let join_start = std::time::Instant::now();

    while !monitor_handle.is_finished() {
        if join_start.elapsed() > join_timeout {
            eprintln!("⚠️ Поток мониторинга не завершился за 2 секунды, принудительное завершение");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("\n🏁 Программа завершена. Общее время: {:.1} сек", start.elapsed().as_secs_f64());

    // Убиваем все дочерние потоки rayon
    drop(monitor_handle);
    std::process::exit(0);
}