//! Drain-lite merge evidence: groups distinct templates that differ at exactly
//! one literal word, and says what the ported evidence rules would call that
//! position.
//!
//!   cargo run --release --example families -- FILE [--show N]
//!
//! `--show` prints family members; never paste its output from a private log.

use std::collections::HashMap;

use siftr_normalize::{Normalizer, SlotClass, SlotKind, SlotStats, classify};

struct Member {
    word: Vec<u8>,
    count: u64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("usage: families FILE [--show N]");
    let show = args
        .iter()
        .position(|a| a == "--show")
        .map_or(0, |i| args[i + 1].parse().expect("number"));

    let data = std::fs::read(path).expect("read");
    let mut n = Normalizer::new();
    let mut templates: HashMap<u64, (u64, Vec<u8>)> = HashMap::new();
    for line in data.split(|&b| b == b'\n') {
        let r = n.normalize(line);
        templates
            .entry(r.template_hash)
            .or_insert_with(|| (0, r.template.to_vec()))
            .0 += 1;
    }

    // Key: template with one literal word cut out, plus the byte before it.
    let mut families: HashMap<Vec<u8>, Vec<Member>> = HashMap::new();
    for (count, t) in templates.values() {
        for (s, e) in literal_words(t) {
            let mut key = Vec::with_capacity(t.len() + 2);
            key.extend_from_slice(&t[..s]);
            key.push(0);
            key.extend_from_slice(&t[e..]);
            families.entry(key).or_default().push(Member {
                word: t[s..e].to_vec(),
                count: *count,
            });
        }
    }
    families.retain(|_, m| m.len() > 1);

    // (position, class) -> (families, lines)
    let mut table: HashMap<(&str, SlotClass), (usize, u64)> = HashMap::new();
    let mut rows = Vec::new();
    for (key, members) in &families {
        let cut = key.iter().position(|&b| b == 0).unwrap();
        let position = match cut.checked_sub(1).map(|i| key[i]) {
            Some(b'/' | b'=') => "value (after / or =)",
            _ => "word",
        };
        let mut stats = SlotStats::new(SlotKind::Quoted);
        for m in members {
            for _ in 0..m.count {
                stats.observe(&m.word);
            }
        }
        let (class, confidence) = classify(&stats);
        let lines: u64 = members.iter().map(|m| m.count).sum();
        let entry = table.entry((position, class)).or_default();
        entry.0 += 1;
        entry.1 += lines;
        rows.push((lines, position, class, confidence, key, members));
    }

    println!("distinct templates {}", templates.len());
    println!("one-word families  {}", families.len());
    let mut summary: Vec<_> = table.into_iter().collect();
    summary.sort_by_key(|row| std::cmp::Reverse(row.1.1));
    for ((position, class), (count, lines)) in summary {
        println!("  {position:<22} {class:<10?} families {count:>5}  lines {lines:>8}");
    }

    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    for (lines, position, class, confidence, key, members) in rows.iter().take(show) {
        let words: Vec<_> = members
            .iter()
            .map(|m| format!("{}×{}", String::from_utf8_lossy(&m.word), m.count))
            .collect();
        println!(
            "{lines:>8} {position} {class:?} {confidence}  {}  [{}]",
            String::from_utf8_lossy(key).replace('\0', "<*>"),
            words.join(", ")
        );
    }
}

/// Spans of purely alphabetic words (placeholders excluded).
fn literal_words(t: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'<'
            && let Some(close) = t[i..].iter().position(|&b| b == b'>')
        {
            i += close + 1;
            continue;
        }
        if t[i].is_ascii_alphabetic() {
            let s = i;
            while i < t.len() && (t[i].is_ascii_alphanumeric() || t[i] == b'_') {
                i += 1;
            }
            if t[s..i]
                .iter()
                .all(|b| b.is_ascii_alphabetic() || *b == b'_')
            {
                out.push((s, i));
            }
        } else {
            i += 1;
        }
    }
    out
}
