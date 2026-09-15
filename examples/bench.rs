//! Throughput, allocations and template sanity on a log file.
//!
//!   cargo run --release --example bench -- FILE [--passes N] [--top N]
//!   cargo run --release --example bench -- FILE --stream   # prototype-comparable
//!
//! `--top` prints the most frequent templates; never paste its output from a
//! private log anywhere.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use siftr::normalize::Normalizer;

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .expect("usage: bench FILE [--passes N] [--top N] [--stream]");
    let flag = |name: &str| args.iter().position(|a| a == name);
    let num = |name: &str, default: usize| {
        flag(name).map_or(default, |i| args[i + 1].parse().expect("number"))
    };

    if flag("--stream").is_some() {
        return stream(path);
    }

    let data = std::fs::read(path).expect("read");
    let lines: Vec<&[u8]> = data.split(|&b| b == b'\n').collect();
    let mb = data.len() as f64 / 1e6;
    let mut n = Normalizer::new();

    let mut times = Vec::new();
    let mut first_pass_allocs = 0;
    let mut steady_allocs = 0;
    let mut slots = 0u64;
    for pass in 0..num("--passes", 5) {
        let before = ALLOCS.load(Ordering::Relaxed);
        let start = Instant::now();
        let mut acc = 0u64;
        let mut pass_slots = 0u64;
        for line in &lines {
            let r = n.normalize(line);
            acc ^= r.template_hash;
            pass_slots += r.slots.len() as u64;
        }
        times.push(start.elapsed().as_secs_f64());
        std::hint::black_box(acc);
        let allocs = ALLOCS.load(Ordering::Relaxed) - before;
        if pass == 0 {
            first_pass_allocs = allocs;
        } else {
            steady_allocs += allocs;
        }
        slots = pass_slots;
    }
    let steady_lines = lines.len() as u64 * (times.len() as u64 - 1).max(1);
    times.sort_by(f64::total_cmp);
    let (best, median) = (times[0], times[times.len() / 2]);

    println!("lines            {}", lines.len());
    println!("bytes            {:.1} MB", mb);
    println!(
        "normalize        best {:.3}s  median {:.3}s over {} passes",
        best,
        median,
        times.len()
    );
    println!(
        "lines/sec        {:.2} M (best)",
        lines.len() as f64 / best / 1e6
    );
    println!("MB/s             {:.0} (best)", mb / best);
    println!(
        "ns/line          {:.0} (best)",
        best * 1e9 / lines.len() as f64
    );
    println!("slots/line       {:.2}", slots as f64 / lines.len() as f64);
    println!(
        "allocs           first pass {first_pass_allocs} ({:.6}/line), steady {steady_allocs} ({:.6}/line)",
        first_pass_allocs as f64 / lines.len() as f64,
        steady_allocs as f64 / steady_lines as f64
    );

    // Template sanity: distinct count, and how many templates exist only
    // because two value-dependent kinds rendered differently.
    let mut templates: HashMap<u64, (u64, Vec<u8>)> = HashMap::new();
    for line in &lines {
        let r = n.normalize(line);
        templates
            .entry(r.template_hash)
            .or_insert_with(|| (0, r.template.to_vec()))
            .0 += 1;
    }
    println!("templates        {}", templates.len());
    for (a, b) in [
        ("<hex>", "<int>"),
        ("<float>", "<int>"),
        ("<version>", "<ip>"),
    ] {
        let erased: std::collections::HashSet<Vec<u8>> = templates
            .values()
            .map(|(_, t)| replace(t, a.as_bytes(), b.as_bytes()))
            .collect();
        let lines_affected: u64 = {
            let mut by_erased: HashMap<Vec<u8>, Vec<u64>> = HashMap::new();
            for (count, t) in templates.values() {
                by_erased
                    .entry(replace(t, a.as_bytes(), b.as_bytes()))
                    .or_default()
                    .push(*count);
            }
            by_erased
                .values()
                .filter(|v| v.len() > 1)
                .map(|v| v.iter().sum::<u64>() - v.iter().max().unwrap())
                .sum()
        };
        println!(
            "kind split {a}/{b}  {} templates, {lines_affected} lines outside their family's main template",
            templates.len() - erased.len()
        );
    }

    let mut sorted: Vec<_> = templates.values().collect();
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let top_share: u64 = sorted.iter().take(20).map(|(c, _)| c).sum();
    let singletons = sorted.iter().filter(|(c, _)| *c == 1).count();
    println!(
        "top 20 cover     {:.1}% of lines; {} templates seen once",
        100.0 * top_share as f64 / lines.len() as f64,
        singletons
    );
    let template_bytes: u64 = templates.values().map(|(c, t)| c * t.len() as u64).sum();
    println!(
        "template bytes   {:.0}/line",
        template_bytes as f64 / lines.len() as f64
    );

    // Share of normalize spent hashing: re-hash every line's template alone.
    let mut all = Vec::with_capacity(template_bytes as usize);
    let mut ends = Vec::with_capacity(lines.len());
    for line in &lines {
        all.extend_from_slice(n.normalize(line).template);
        ends.push(all.len());
    }
    let mut hash_best = f64::MAX;
    for _ in 0..5 {
        let start = Instant::now();
        let (mut acc, mut from) = (0u64, 0);
        for &end in &ends {
            acc ^= siftr::normalize::fnv1a64(&all[from..end]);
            from = end;
        }
        std::hint::black_box(acc);
        hash_best = hash_best.min(start.elapsed().as_secs_f64());
    }
    println!(
        "hash             {:.0} ns/line ({:.0}% of normalize best)",
        hash_best * 1e9 / lines.len() as f64,
        100.0 * hash_best / best
    );
    for (count, t) in sorted.iter().take(num("--top", 0)) {
        println!("{count:>8}  {}", String::from_utf8_lossy(t));
    }
    // Under-masking hides in the long tail: sample templates seen once.
    let want = num("--singletons", 0);
    let singles: Vec<_> = sorted.iter().filter(|(c, _)| *c == 1).collect();
    if let Some(step) = singles.len().checked_div(want) {
        for (_, t) in singles.iter().step_by(step.max(1)).take(want) {
            println!("       1  {}", String::from_utf8_lossy(t));
        }
    }
}

/// Streaming read + normalize + distinct-template count, shaped like the
/// survey prototype so `/usr/bin/time -l` numbers compare.
fn stream(path: &str) {
    let file = std::fs::File::open(path).expect("open");
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut line = Vec::with_capacity(512);
    let mut n = Normalizer::new();
    let mut templates: HashMap<u64, u64> = HashMap::new();
    let mut slots = 0u64;
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line).expect("read") == 0 {
            break;
        }
        let out = n.normalize(&line);
        slots += out.slots.len() as u64;
        *templates.entry(out.template_hash).or_insert(0) += 1;
    }
    eprintln!("templates={} slots={}", templates.len(), slots);
}

fn replace(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}
