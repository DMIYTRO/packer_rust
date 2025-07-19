use std::collections::HashMap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use clap::Parser;

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
    counts: Vec<u32>,
}

#[derive(Debug, Clone)]
struct Solution {
    items: Vec<PlacedItem>,
    remaining_counts: Vec<u32>,
    total_area: u32,
}

type MemoCache = HashMap<MemoKey, Solution>;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Page width in mm
    #[arg(short = 'w', long, default_value = "749")]
    page_width: u32,

    /// Page height in mm
    #[arg(long, default_value = "588")]
    page_height: u32,

    /// JSON file with element types (optional, uses default if not provided)
    #[arg(short = 'f', long)]
    file: Option<String>,
}

fn solve_guillotine_memoized(
    width: u32,
    height: u32,
    items_counts: &[u32],
    item_specs: &[ElementType],
    
    memo: &mut MemoCache,
) -> Solution {
    let memo_key = MemoKey {
        width,
        height,
        counts: items_counts.to_vec(),
    };

    if let Some(cached) = memo.get(&memo_key) {
        return cached.clone();
    }

    if items_counts.iter().all(|&count| count == 0) || width == 0 || height == 0 {
        let solution = Solution {
            items: Vec::new(),
            remaining_counts: items_counts.to_vec(),
            total_area: 0,
        };
        memo.insert(memo_key, solution.clone());
        return solution;
    }

    let mut best_solution = Solution {
        items: Vec::new(),
        remaining_counts: items_counts.to_vec(),
        total_area: 0,
    };

    for (item_idx, spec) in item_specs.iter().enumerate() {
        if items_counts[item_idx] == 0 {
            continue;
        }

        let item_master_w = spec.width;
        let item_master_h = spec.height;

        // Try both orientations
        for rotation_idx in 0..2 {
            let (item_w, item_h, is_rotated) = if rotation_idx == 0 {
                (item_master_w, item_master_h, false)
            } else {
                if item_master_w == item_master_h {
                    continue; // Skip rotating squares
                }
                (item_master_h, item_master_w, true)
            };

            if item_w <= width && item_h <= height {
                let placed_item = PlacedItem {
                    x: 0,
                    y: 0,
                    width: item_w,
                    height: item_h,
                    name: spec.name.clone(),
                    rotated: is_rotated,
                };

                let current_item_area = item_w * item_h;

                let mut new_counts = items_counts.to_vec();
                new_counts[item_idx] -= 1;

                // Strategy 1: Horizontal cut
                let res1 = solve_guillotine_memoized(
                    width - item_w,
                    item_h,
                    &new_counts,
                    item_specs,
                    memo,
                );

                let res2 = solve_guillotine_memoized(
                    width,
                    height - item_h,
                    &res1.remaining_counts,
                    item_specs,
                    memo,
                );

                let total_area_s1 = current_item_area + res1.total_area + res2.total_area;

                if total_area_s1 > best_solution.total_area {
                    let mut final_placed_s1 = vec![placed_item.clone()];

                    for mut p in res1.items {
                        p.x += item_w;
                        final_placed_s1.push(p);
                    }

                    for mut p in res2.items {
                        p.y += item_h;
                        final_placed_s1.push(p);
                    }

                    best_solution = Solution {
                        items: final_placed_s1,
                        remaining_counts: res2.remaining_counts,
                        total_area: total_area_s1,
                    };
                }

                // Strategy 2: Vertical cut
                let res_a = solve_guillotine_memoized(
                    item_w,
                    height - item_h,
                    &new_counts,
                    item_specs,
                    memo,
                );

                let res_b = solve_guillotine_memoized(
                    width - item_w,
                    height,
                    &res_a.remaining_counts,
                    item_specs,
                    memo,
                );

                let total_area_s2 = current_item_area + res_a.total_area + res_b.total_area;

                if total_area_s2 > best_solution.total_area {
                    let mut final_placed_s2 = vec![placed_item];

                    for mut p in res_a.items {
                        p.y += item_h;
                        final_placed_s2.push(p);
                    }

                    for mut p in res_b.items {
                        p.x += item_w;
                        final_placed_s2.push(p);
                    }

                    best_solution = Solution {
                        items: final_placed_s2,
                        remaining_counts: res_b.remaining_counts,
                        total_area: total_area_s2,
                    };
                }
            }
        }
    }

    memo.insert(memo_key, best_solution.clone());
    best_solution
}

fn solve_first_level(
    page_width: u32,
    page_height: u32,
    item_specs: &[ElementType],
    initial_counts: &[u32],
    first_item_idx: usize,
    first_item_rotated: bool,
) -> (Vec<PlacedItem>, u32) {
    let mut memo = HashMap::new();

    let spec = &item_specs[first_item_idx];
    let (item_w, item_h) = if first_item_rotated {
        (spec.height, spec.width)
    } else {
        (spec.width, spec.height)
    };

    if item_w > page_width || item_h > page_height {
        return (Vec::new(), 0);
    }

    let placed_first_item = PlacedItem {
        x: 0,
        y: 0,
        width: item_w,
        height: item_h,
        name: spec.name.clone(),
        rotated: first_item_rotated,
    };

    let current_item_area = item_w * item_h;

    let mut new_counts = initial_counts.to_vec();
    new_counts[first_item_idx] -= 1;

    // Strategy 1: Horizontal cut
    let res1 = solve_guillotine_memoized(
        page_width - item_w,
        item_h,
        &new_counts,
        item_specs,
        &mut memo,
    );

    let res2 = solve_guillotine_memoized(
        page_width,
        page_height - item_h,
        &res1.remaining_counts,
        item_specs,
        &mut memo,
    );

    let mut solution1_items = vec![placed_first_item.clone()];
    for mut p in res1.items {
        p.x += item_w;
        solution1_items.push(p);
    }
    for mut p in res2.items {
        p.y += item_h;
        solution1_items.push(p);
    }
    let solution1_area = current_item_area + res1.total_area + res2.total_area;

    // Strategy 2: Vertical cut
    let res_a = solve_guillotine_memoized(
        item_w,
        page_height - item_h,
        &new_counts,
        item_specs,
        &mut memo,
    );

    let res_b = solve_guillotine_memoized(
        page_width - item_w,
        page_height,
        &res_a.remaining_counts,
        item_specs,
        &mut memo,
    );

    let mut solution2_items = vec![placed_first_item];
    for mut p in res_a.items {
        p.y += item_h;
        solution2_items.push(p);
    }
    for mut p in res_b.items {
        p.x += item_w;
        solution2_items.push(p);
    }
    let solution2_area = current_item_area + res_a.total_area + res_b.total_area;

    if solution1_area > solution2_area {
        (solution1_items, solution1_area)
    } else {
        (solution2_items, solution2_area)
    }
}

#[derive(Debug, Serialize)]
struct OptimizationResult {
    scheme: Vec<PlacedItem>,
    placed_elements: HashMap<String, u32>,
    unplaced_elements: HashMap<String, u32>,
    effective_area: f64,
    total_placed_area: u32,
    execution_time_ms: u128,
}

fn optimize_layout_guillotine_parallel(
    page_width: u32,
    page_height: u32,
    element_types: &[ElementType],
) -> OptimizationResult {
    let start_time = std::time::Instant::now();

    // Sort by area for better heuristics
    let mut item_specs = element_types.to_vec();
    item_specs.sort_by(|a, b| {
        (b.width * b.height).cmp(&(a.width * a.height))
    });

    let initial_counts: Vec<u32> = item_specs.iter().map(|item| item.count).collect();

    // Generate tasks for parallel processing
    let mut tasks = Vec::new();
    for (item_idx, spec) in item_specs.iter().enumerate() {
        if initial_counts[item_idx] > 0 {
            let w = spec.width;
            let h = spec.height;

            if w <= page_width && h <= page_height {
                tasks.push((item_idx, false));
            }

            if w != h && h <= page_width && w <= page_height {
                tasks.push((item_idx, true));
            }
        }
    }

    if tasks.is_empty() {
        let execution_time = start_time.elapsed().as_millis();
        let unplaced_elements: HashMap<String, u32> = element_types
            .iter()
            .map(|el| (el.name.clone(), el.count))
            .collect();

        return OptimizationResult {
            scheme: Vec::new(),
            placed_elements: HashMap::new(),
            unplaced_elements,
            effective_area: 0.0,
            total_placed_area: 0,
            execution_time_ms: execution_time,
        };
    }

    // Parallel execution
    let results: Vec<(Vec<PlacedItem>, u32)> = tasks
        .par_iter()
        .map(|&(item_idx, rotated)| {
            solve_first_level(
                page_width,
                page_height,
                &item_specs,
                &initial_counts,
                item_idx,
                rotated,
            )
        })
        .collect();

    // Find best result
    let (best_scheme, best_area) = results
        .into_iter()
        .max_by_key(|(_, area)| *area)
        .unwrap_or((Vec::new(), 0));

    // Count placed and unplaced elements
    let mut placed_counts: HashMap<String, u32> = HashMap::new();
    for item in &best_scheme {
        *placed_counts.entry(item.name.clone()).or_insert(0) += 1;
    }

    let mut unplaced_counts: HashMap<String, u32> = HashMap::new();
    for spec in element_types {
        let placed = placed_counts.get(&spec.name).unwrap_or(&0);
        let unplaced = spec.count - placed;
        if unplaced > 0 {
            unplaced_counts.insert(spec.name.clone(), unplaced);
        }
    }

    let total_sheet_area = page_width * page_height;
    let efficiency = if total_sheet_area > 0 {
        (best_area as f64 / total_sheet_area as f64) * 100.0
    } else {
        0.0
    };

    let execution_time = start_time.elapsed().as_millis();

    OptimizationResult {
        scheme: best_scheme,
        placed_elements: placed_counts,
        unplaced_elements: unplaced_counts,
        effective_area: efficiency,
        total_placed_area: best_area,
        execution_time_ms: execution_time,
    }
}

fn get_default_elements() -> Vec<ElementType> {
    vec![
        ElementType {
            width: 42,
            height: 32,
            count: 1,
            name: "40x30".to_string(),
        },
        ElementType {
            width: 87,
            height: 82,
            count: 2,
            name: "85x80".to_string(),
        },
        ElementType {
            width: 92,
            height: 52,
            count: 48,
            name: "90x50".to_string(),
        },
        ElementType {
            width: 102,
            height: 72,
            count: 1,
            name: "100x70".to_string(),
        },
        ElementType {
            width: 150,
            height: 107,
            count: 2,
            name: "148x105".to_string(),
        },
        ElementType {
            width: 170,
            height: 132,
            count: 1,
            name: "168x130".to_string(),
        },
    ]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let element_types = if let Some(file_path) = args.file {
        let file_content = std::fs::read_to_string(file_path)?;
        serde_json::from_str(&file_content)?
    } else {
        get_default_elements()
    };

    println!("Starting guillotine cutting optimization...");
    println!("Page size: {}x{} mm", args.page_width, args.page_height);
    println!("Element types:");
    for el in &element_types {
        println!("  - {}: {}x{} mm, count: {}", el.name, el.width, el.height, el.count);
    }
    println!();

    let result = optimize_layout_guillotine_parallel(
        args.page_width,
        args.page_height,
        &element_types,
    );

    // Print results
    let total_sheet_area = args.page_width * args.page_height;
    let unfilled_area = total_sheet_area - result.total_placed_area;

    println!("{}", "=".repeat(50));
    println!("           CUTTING RESULTS");
    println!("{}", "=".repeat(50));
    println!();
    println!("📊 OVERALL EFFICIENCY:");
    println!("  -> Fill percentage: {:.2}%", result.effective_area);
    println!("  -> Total sheet area: {} mm²", total_sheet_area);
    println!("  -> Placed elements area: {} mm²", result.total_placed_area);
    println!("  -> Remaining area: {} mm²", unfilled_area);
    println!("  -> Execution time: {} ms", result.execution_time_ms);
    println!();

    println!("✅ PLACED ELEMENTS:");
    if !result.placed_elements.is_empty() {
        let total_placed: u32 = result.placed_elements.values().sum();
        println!("  Total placed: {} pcs", total_placed);
        for (element_type, count) in &result.placed_elements {
            println!("    - Type '{}': {} pcs", element_type, count);
        }
    } else {
        println!("  Nothing placed.");
    }
    println!();

    println!("❌ UNPLACED ELEMENTS:");
    if !result.unplaced_elements.is_empty() {
        let total_unplaced: u32 = result.unplaced_elements.values().sum();
        println!("  Total unplaced: {} pcs", total_unplaced);
        for (element_type, count) in &result.unplaced_elements {
            println!("    - Type '{}': {} pcs", element_type, count);
        }
    } else {
        println!("  All elements were placed.");
    }
    println!();
    println!("{}", "=".repeat(50));

    // Save results to JSON file
    let output_file = "cutting_result.json";
    let json_output = serde_json::to_string_pretty(&result)?;
    std::fs::write(output_file, json_output)?;
    println!("Results saved to: {}", output_file);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_optimization() {
        let elements = vec![
            ElementType {
                width: 100,
                height: 50,
                count: 2,
                name: "test1".to_string(),
            },
            ElementType {
                width: 50,
                height: 50,
                count: 1,
                name: "test2".to_string(),
            },
        ];

        let result = optimize_layout_guillotine_parallel(200, 100, &elements);

        assert!(result.total_placed_area > 0);
        assert!(result.effective_area > 0.0);
    }
}
