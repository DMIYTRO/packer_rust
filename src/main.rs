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
    #[arg(short = 'j', long, default_value = "0")]
    threads: usize,
    /// Лимит (сек) на goal-driven фазу принудительного размещения. 0 = пропустить.
    #[arg(short = 'g', long, default_value = "90")]
    goal_driven_seconds: u64,
}

// Глобальные переменные мониторинга
static GLOBAL_BEST_AREA: AtomicU64 = AtomicU64::new(0);
static GLOBAL_BEST_COUNT: AtomicU32 = AtomicU32::new(0);
static FOUND_PERFECT: AtomicBool = AtomicBool::new(false);
static BRANCHES_PROCESSED: AtomicU32 = AtomicU32::new(0);
static TOTAL_ITERATIONS: AtomicU64 = AtomicU64::new(0);
static TIMEOUT_REACHED: AtomicBool = AtomicBool::new(false);

// Доля (%) узлов, где blink-диверсификация исключает лучшего кандидата из луча.
const BLINK_PCT: u64 = 15;

// Быстрый детерминированный хэш для blink-перемешивания (без внешних крейтов).
#[inline]
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

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
    blink_seed: u64,
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

    // Blink-диверсификация (GDRR): с вероятностью BLINK_PCT% исключаем лучшего
    // кандидата из луча. Разные seed -> разные ветви поиска при перезапусках.
    if blink_seed != 0 && candidates.len() >= 2 {
        let h = splitmix64(blink_seed
            ^ (width as u64).wrapping_mul(0x9E3779B97F4A7C15)
            ^ (height as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)
            ^ (depth as u64).wrapping_mul(0x165667B19E3779F9)
            ^ total_remaining as u64);
        if h % 100 < BLINK_PCT {
            let first = candidates.remove(0);
            candidates.push(first);
        }
    }

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
                solve_recursive(width - item_w, item_h, &new_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width, blink_seed).and_then(|res_r| {
                    solve_recursive(width, height - item_h, &res_r.remaining_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width, blink_seed).map(|res_b| {
                        let total = item_area + res_r.total_area + res_b.total_area;
                        let mut items = vec![PlacedItem { x: 0, y: 0, width: item_w, height: item_h, name: item_specs[idx].name.clone(), rotated }];
                        for mut p in res_r.items { p.x += item_w; items.push(p); }
                        for mut p in res_b.items { p.y += item_h; items.push(p); }
                        Solution { items, remaining_counts: res_b.remaining_counts, total_area: total }
                    })
                })
            } else {
                solve_recursive(item_w, height - item_h, &new_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width, blink_seed).and_then(|res_b| {
                    solve_recursive(width - item_w, height, &res_b.remaining_counts, item_specs, memo, depth + 1, start_time, timeout, beam_width, blink_seed).map(|res_r| {
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

            'search: for y in 0..=page_height.saturating_sub(h) {
                for x in 0..=page_width.saturating_sub(w) {
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

#[derive(Clone, Debug)]
struct Rect {
    x: u32, y: u32, w: u32, h: u32,
}

fn get_max_free_rects(
    page_width: u32,
    page_height: u32,
    placed: &[PlacedItem],
) -> Vec<Rect> {
    let mut free_rects = vec![Rect { x: 0, y: 0, w: page_width, h: page_height }];
    
    for p in placed {
        let mut new_free = Vec::new();
        for f in free_rects {
            if p.x < f.x + f.w && p.x + p.width > f.x &&
               p.y < f.y + f.h && p.y + p.height > f.y {
                if p.x > f.x { new_free.push(Rect { x: f.x, y: f.y, w: p.x - f.x, h: f.h }); }
                if p.x + p.width < f.x + f.w { new_free.push(Rect { x: p.x + p.width, y: f.y, w: f.x + f.w - (p.x + p.width), h: f.h }); }
                if p.y > f.y { new_free.push(Rect { x: f.x, y: f.y, w: f.w, h: p.y - f.y }); }
                if p.y + p.height < f.y + f.h { new_free.push(Rect { x: f.x, y: p.y + p.height, w: f.w, h: f.y + f.h - (p.y + p.height) }); }
            } else {
                new_free.push(f);
            }
        }
        
        let mut maximal = Vec::new();
        for i in 0..new_free.len() {
            let mut is_contained = false;
            for j in 0..new_free.len() {
                if i != j {
                    let r1 = &new_free[i];
                    let r2 = &new_free[j];
                    if r1.x >= r2.x && r1.y >= r2.y && r1.x + r1.w <= r2.x + r2.w && r1.y + r1.h <= r2.y + r2.h {
                        is_contained = true;
                        break;
                    }
                }
            }
            if !is_contained { maximal.push(new_free[i].clone()); }
        }
        free_rects = maximal;
    }
    free_rects
}

fn pack_remaining_items_backtracking(
    page_width: u32,
    page_height: u32,
    placed: &[PlacedItem],
    specs: &[ElementType],
    remaining: &mut [u32],
    iters: &mut u32,
) -> Option<Vec<PlacedItem>> {
    *iters += 1;
    if *iters > 50_000 { return None; }

    let unplaced: u32 = remaining.iter().sum();
    if unplaced == 0 { return Some(Vec::new()); }
    
    let mut free_rects = get_max_free_rects(page_width, page_height, placed);
    free_rects.sort_by_key(|f| (f.y, f.x));
    
    let mut best_spec_idx = None;
    let mut best_area = 0;
    for (i, &count) in remaining.iter().enumerate() {
        if count > 0 {
            let area = specs[i].width * specs[i].height;
            if area > best_area {
                best_area = area;
                best_spec_idx = Some(i);
            }
        }
    }
    
    let spec_idx = best_spec_idx.unwrap();
    let spec = &specs[spec_idx];
    
    for &(w, h, rot) in &[(spec.width, spec.height, false), (spec.height, spec.width, true)] {
        if w == h && rot { continue; }
        
        for f in &free_rects {
            if w <= f.w && h <= f.h {
                let p = PlacedItem { x: f.x, y: f.y, width: w, height: h, name: spec.name.clone(), rotated: rot };
                let mut new_placed = placed.to_vec();
                new_placed.push(p.clone());
                remaining[spec_idx] -= 1;
                
                if let Some(mut rest) = pack_remaining_items_backtracking(page_width, page_height, &new_placed, specs, remaining, iters) {
                    rest.push(p);
                    remaining[spec_idx] += 1;
                    return Some(rest);
                }
                remaining[spec_idx] += 1;
            }
        }
    }
    None
}

fn try_local_swap(
    page_width: u32,
    page_height: u32,
    placed: &mut Vec<PlacedItem>,
    specs: &[ElementType],
    remaining: &mut Vec<u32>,
) -> bool {
    let unplaced: u32 = remaining.iter().sum();
    if unplaced == 0 { return false; }

    println!("   [DP POST-PROCESSING] Start Large Neighborhood Search with MaxRects...");
    use rand::Rng;
    let mut rng = rand::thread_rng();

    for iter in 0..1000 {
        let window_w = rng.gen_range(50..350);
        let window_h = rng.gen_range(50..350);
        let wx = rng.gen_range(0..=page_width.saturating_sub(window_w));
        let wy = rng.gen_range(0..=page_height.saturating_sub(window_h));
        
        let mut removed_items = Vec::new();
        let mut keep_items = Vec::new();
        
        for item in placed.iter() {
            if item.x < wx + window_w && item.x + item.width > wx &&
               item.y < wy + window_h && item.y + item.height > wy {
                removed_items.push(item.clone());
            } else {
                keep_items.push(item.clone());
            }
        }
        
        let current_unplaced: u32 = unplaced + removed_items.len() as u32;
        if current_unplaced > 12 { continue; } 
        
        let mut temp_remaining = remaining.clone();
        for item in &removed_items {
            if let Some(spec_idx) = specs.iter().position(|s| s.name == item.name) {
                temp_remaining[spec_idx] += 1;
            }
        }
        
        if let Some(packed) = pack_remaining_items_backtracking(page_width, page_height, &keep_items, specs, &mut temp_remaining, &mut 0) {
            println!("   [УСПЕХ] MaxRects LNS packed all {} items after {} iterations!", current_unplaced, iter);
            *placed = keep_items;
            placed.extend(packed);
            for c in remaining.iter_mut() { *c = 0; }
            return true;
        }
    }
    false
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
    // Атомарная запись: пишем во временный файл, синхронизируем на диск и
    // переименовываем. rename атомарен в пределах ФС, поэтому читатель (веб)
    // никогда не увидит обрезанный JSON — даже если процесс прибьют по таймауту.
    // Имя .tmp УНИКАЛЬНО на каждую запись: save_result вызывается параллельно из
    // множества rayon-потоков, и общий .tmp приводил бы к перемешанным записям.
    static SAVE_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SAVE_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_file = format!("{}.{}.{}.tmp", out_file, std::process::id(), seq ^ nanos as u64);
    if let Ok(mut file) = std::fs::File::create(&tmp_file) {
        if serde_json::to_writer_pretty(&mut file, &output).is_ok() {
            let _ = file.sync_all(); // Гарантируем запись на диск
            drop(file);
            if std::fs::rename(&tmp_file, &out_file).is_err() {
                let _ = std::fs::remove_file(&tmp_file);
            }
        } else {
            drop(file);
            let _ = std::fs::remove_file(&tmp_file);
        }
    }
}

// ============================================================================
// GOAL-DRIVEN FORCED PLACEMENT
// ============================================================================
// Принудительно ставит трудную деталь первым ходом в угол листа, режет лист на
// 2 подпрямоугольника и заполняет их полным перебором с blink-диверсификацией.
// Множество параллельных перезапусков с разными seed дают разные раскладки —
// это позволяет разместить деталь, недостижимую для детерминированного поиска.
fn goal_driven_forced(
    page_w: u32,
    page_h: u32,
    specs: &[ElementType],
    full_counts: &[u32],
    forced_indices: &[usize],
    beam_width: usize,
    deadline: Duration,
) -> Option<Solution> {
    let total_items: u32 = full_counts.iter().sum();
    let target_area = page_w as u64 * page_h as u64;

    // Конфигурации: трудная деталь × ориентация × направление гильотинного реза.
    let mut configs: Vec<(usize, u32, u32, bool, bool)> = Vec::new();
    for &fidx in forced_indices {
        let (w, h) = (specs[fidx].width, specs[fidx].height);
        for &(fw, fh, rot) in &[(w, h, false), (h, w, true)] {
            if fw > page_w || fh > page_h { continue; }
            if rot && w == h { continue; }
            configs.push((fidx, fw, fh, rot, true));   // рез по вертикали
            configs.push((fidx, fw, fh, rot, false));  // рез по горизонтали
        }
    }
    if configs.is_empty() { return None; }

    // После основного поиска флаги выставлены в true — сбрасываем, иначе
    // solve_recursive внутри немедленно вернёт None.
    FOUND_PERFECT.store(false, Ordering::SeqCst);
    TIMEOUT_REACHED.store(false, Ordering::SeqCst);

    let start = Instant::now();
    let best: Arc<Mutex<Option<Solution>>> = Arc::new(Mutex::new(None));

    let mut seed_base: u64 = 1;
    let batch: u64 = 48;
    while start.elapsed() < deadline && !FOUND_PERFECT.load(Ordering::Relaxed) {
        let jobs: Vec<(u64, usize)> = (seed_base..seed_base + batch)
            .flat_map(|s| (0..configs.len()).map(move |ci| (s, ci)))
            .collect();

        jobs.into_par_iter().for_each(|(seed, ci)| {
            if start.elapsed() > deadline || FOUND_PERFECT.load(Ordering::Relaxed) { return; }
            let (fidx, fw, fh, rotated, cut_v) = configs[ci];
            let mut lc = full_counts.to_vec();
            lc[fidx] -= 1;
            let mut memo: MemoCache = HashMap::new();
            let item_area = fw as u64 * fh as u64;

            let res = if cut_v {
                solve_recursive(page_w - fw, fh, &lc, specs, &mut memo, 1, &start, &deadline, beam_width, seed).and_then(|rr| {
                    solve_recursive(page_w, page_h - fh, &rr.remaining_counts, specs, &mut memo, 1, &start, &deadline, beam_width, seed).map(|rb| {
                        let mut items = vec![PlacedItem { x: 0, y: 0, width: fw, height: fh, name: specs[fidx].name.clone(), rotated }];
                        for mut p in rr.items { p.x += fw; items.push(p); }
                        for mut p in rb.items { p.y += fh; items.push(p); }
                        Solution { items, remaining_counts: rb.remaining_counts, total_area: item_area + rr.total_area + rb.total_area }
                    })
                })
            } else {
                solve_recursive(fw, page_h - fh, &lc, specs, &mut memo, 1, &start, &deadline, beam_width, seed).and_then(|rb| {
                    solve_recursive(page_w - fw, page_h, &rb.remaining_counts, specs, &mut memo, 1, &start, &deadline, beam_width, seed).map(|rr| {
                        let mut items = vec![PlacedItem { x: 0, y: 0, width: fw, height: fh, name: specs[fidx].name.clone(), rotated }];
                        for mut p in rb.items { p.y += fh; items.push(p); }
                        for mut p in rr.items { p.x += fw; items.push(p); }
                        Solution { items, remaining_counts: rr.remaining_counts, total_area: item_area + rb.total_area + rr.total_area }
                    })
                })
            };

            if let Some(sol) = res {
                let placed = sol.items.len();
                let mut g = best.lock().unwrap();
                let better = match g.as_ref() {
                    None => true,
                    Some(b) => placed > b.items.len() || (placed == b.items.len() && sol.total_area > b.total_area),
                };
                if better {
                    let all_placed = placed as u32 == total_items;
                    let eff = sol.total_area as f64 / target_area as f64 * 100.0;
                    println!("\n   [GOAL-DRIVEN] Деталей: {}/{} | Заполнение: {:.4}% (seed {})", placed, total_items, eff, seed);
                    *g = Some(sol);
                    if all_placed { FOUND_PERFECT.store(true, Ordering::SeqCst); }
                }
            }
        });

        seed_base += batch;
    }

    let result = best.lock().unwrap().clone();
    TIMEOUT_REACHED.store(true, Ordering::SeqCst);
    result
}

fn main() {
    let args = Args::parse();

    // Настраиваем пул потоков rayon
    if args.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build_global()
            .expect("Ошибка инициализации пула потоков");
        println!("Потоков: {}", args.threads);
    } else {
        println!("Потоков: {} (авто)", rayon::current_num_threads());
    }

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
                    println!("\n   Таймаут {} сек достигнут!", timeout_secs);
                    break;
                }
            }
        })
    };

    for (task_i, cfg) in configs.iter().enumerate() {
        // Проверяем таймаут перед началом задачи
        if TIMEOUT_REACHED.load(Ordering::Relaxed) {
            println!("\nТаймаут достигнут. Пропуск оставшихся задач.");
            break;
        }

        let task_id = task_i + 1;
        let total_items_to_place: u32 = cfg.elements.iter().map(|e| e.count).sum();
        println!("\nЗАДАЧА #{} [{}x{}]", task_id, cfg.page_width, cfg.page_height);
        println!("   Цель: разместить {} деталей.", total_items_to_place);
        println!("   Останавливаемся при: таймаут {}с | все детали размещены | заполнение ≥99%", args.timeout_seconds);

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
                    solve_recursive(cfg.page_width - item_w, item_h, &local_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width, 0).and_then(|res_r| {
                        solve_recursive(cfg.page_width, cfg.page_height - item_h, &res_r.remaining_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width, 0).map(|res_b| {
                            let mut items = vec![PlacedItem { x: 0, y: 0, width: item_w, height: item_h, name: specs[idx].name.clone(), rotated }];
                            for mut p in res_r.items { p.x += item_w; items.push(p); }
                            for mut p in res_b.items { p.y += item_h; items.push(p); }
                            Solution { items, remaining_counts: res_b.remaining_counts, total_area: (item_w * item_h) as u64 + res_r.total_area + res_b.total_area }
                        })
                    })
                } else {
                    solve_recursive(item_w, cfg.page_height - item_h, &local_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width, 0).and_then(|res_b| {
                        solve_recursive(cfg.page_width - item_w, cfg.page_height, &res_b.remaining_counts, &specs, &mut local_memo, 1, &start, &timeout, args.beam_width, 0).map(|res_r| {
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

                        // Останавливаемся при 100% заполнении или ≥99%
                        if sol.total_area >= target_area * 99 / 100 {
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

        // Локальный swap: убрать 1 деталь и заменить на 2 меньших
        {
            let swapped = try_local_swap(
                cfg.page_width, cfg.page_height,
                &mut final_sol.items, &specs, &mut final_sol.remaining_counts,
            );
            if swapped {
                final_sol.total_area = final_sol.items.iter()
                    .map(|p| p.width as u64 * p.height as u64).sum();
                println!("   [SWAP] Локальное улучшение нашло лучшую раскладку!");
            }
        }

        // GOAL-DRIVEN: принудительное размещение нерешённых деталей с blink-диверсификацией
        let still_unplaced: u32 = final_sol.remaining_counts.iter().sum();
        if still_unplaced > 0 && args.goal_driven_seconds > 0 {
            let forced: Vec<usize> = final_sol.remaining_counts.iter()
                .enumerate().filter(|(_, &c)| c > 0).map(|(i, _)| i).collect();
            let gd_deadline = Duration::from_secs(args.goal_driven_seconds);
            let gd_beam = args.beam_width.max(16);
            println!("\n   [GOAL-DRIVEN] Принудительное размещение {} нерешённых деталей (до {}с, beam {})...",
                still_unplaced, gd_deadline.as_secs(), gd_beam);
            if let Some(gd) = goal_driven_forced(cfg.page_width, cfg.page_height, &specs, &counts, &forced, gd_beam, gd_deadline) {
                let better = gd.items.len() > final_sol.items.len()
                    || (gd.items.len() == final_sol.items.len() && gd.total_area > final_sol.total_area);
                if better {
                    println!("   [GOAL-DRIVEN] УЛУЧШЕНИЕ: {} -> {} деталей | {} -> {} мм²",
                        final_sol.items.len(), gd.items.len(), final_sol.total_area, gd.total_area);
                    final_sol = gd;
                } else {
                    println!("   [GOAL-DRIVEN] Улучшения не найдено (лучшее форсингом: {}/{} деталей).",
                        gd.items.len(), total_items_to_place);
                }
            } else {
                println!("   [GOAL-DRIVEN] Решений не получено.");
            }
        }

        let final_eff = (final_sol.total_area as f64 / target_area as f64) * 100.0;
        println!("\nЗАДАЧА #{} ЗАВЕРШЕНА. Итог: {:.4}%", task_id, final_eff);

        // ВСЕГДА сохраняем финальный результат
        save_result(task_id, &final_sol, target_area);
        println!("   Результат сохранён в result_task_{}.json", task_id);

        // Автоматически запускаем визуализацию
        let result_file = format!("result_task_{}.json", task_id);
        let task_idx = (task_id - 1).to_string();
        let vis_status = std::process::Command::new("python3")
            .args(["visualize.py", &result_file, "--config", &args.file, "--task", &task_idx])
            .status();
        match vis_status {
            Ok(s) if s.success() => println!("   Схема сохранена в result_task_{}.png", task_id),
            Ok(_) => eprintln!("   visualize.py завершился с ошибкой"),
            Err(e) => eprintln!("   Не удалось запустить python3: {}", e),
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
            eprintln!("Поток мониторинга не завершился за 2 секунды, принудительное завершение");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("\nПрограмма завершена. Общее время: {:.1} сек", start.elapsed().as_secs_f64());

    // Убиваем все дочерние потоки rayon
    drop(monitor_handle);
    std::process::exit(0);
}
