#!/usr/bin/env ruby
# Noise measurement and leave-one-out backtest of the signal rules in
# ../signals.md, over data/ produced by extract.rb.
#   analyze.rb [noise|sweep|backtest|inject|all]
require "json"
require "csv"

DATA = File.join(__dir__, "data")
RAW = JSON.parse(File.read(File.join(DATA, "runs.json")))
def template_names(h) = h.transform_keys { RAW["templates"][_1.to_i] }
RUNS = RAW["runs"].each do |r|
  r["suite_sql"] = template_names(r["suite_sql"]) if r["suite_sql"]
  (r["examples"] || []).each { _1["sql"] = template_names(_1["sql"]) if _1["sql"] }
end

def median(a)
  s = a.sort
  n = s.size
  n.odd? ? s[n / 2] : (s[n / 2 - 1] + s[n / 2]) / 2.0
end

def mad(a)
  m = median(a)
  median(a.map { (_1 - m).abs })
end

def q(a, p) = a.sort[((a.size - 1) * p).round]
def sig(x, d = 3) = x.zero? ? 0 : x.round(d - 1 - Math.log10(x.abs).floor)

# ---------------------------------------------------------------- behaviors
SETUP = "(setup)"

# One hash per run: behavior id => measures, mirroring siftr aggregates.
def add_count(b, id, kind, n, example = nil, field = "count")
  s = (b[id] ||= { "kind" => kind, "count" => 0, "by_example" => Hash.new(0) })
  s[field] = (s[field] || 0) + n
  s["by_example"][example] += n if example
end

def behaviors(run)
  b = {}
  run["stderr"].each { |t, n| b["stderr:#{t}"] = { "kind" => "stderr", "count" => n } }
  (run["examples"] || []).each do |e|
    q = (e["sql"] || {}).reject { |k, _| k.start_with?("TRANSACTION") }
    b["example:#{e['id']}"] = { "kind" => "example", "name" => e["name"], "seq" => e["seq"], "count" => 1,
                                "status" => e["status"], "exception" => e["exception"],
                                "ms" => e["run_time"] * 1000, "queries" => q.values.sum }
    q.each { |t, n| add_count(b, "sql:#{t}", "sql", n, e["id"]) }
    (e["requests"] || {}).each do |action, r|
      add_count(b, "request:#{action}", "request", r["count"], e["id"])
      req = b["request:#{action}"]
      req["queries"] = (req["queries"] || 0) + r["queries"]
      (req["queries_by_example"] ||= Hash.new(0))[e["id"]] += r["queries"]
    end
  end
  # Log lines before the first example: boot, schema checks, fixture loads.
  (run["suite_sql"] || {}).each { |t, n| add_count(b, "sql:#{t}", "sql", n, SETUP) }
  b["suite"] = { "kind" => "suite", "count" => 1, "ms" => run["duration"] * 1000 } if run["duration"]
  b
end

IRIQ = begin
  csv = CSV.read(File.join(DATA, "iriq_run_time.csv"), headers: true)
  csv.headers.drop(1).to_h { |c| [c, csv.each_with_index.to_h { |row, i| [row["id"], [i, row[c]&.to_i]] }] }
rescue Errno::ENOENT
  {}
end

def iriq_behaviors(run)
  times = IRIQ.fetch("#{run['scenario']}/#{format('%03d', run['idx'])}")
  b = times.to_h do |id, (seq, us)|
    ["example:#{id}", { "kind" => "example", "name" => id, "seq" => seq, "count" => 1, "status" => "passed", "ms" => us / 1000.0 }]
  end
  b["suite"] = { "kind" => "suite", "count" => 1, "ms" => run["duration"] * 1000 }
  b
end

def load_suite(suite)
  @suites ||= {}
  @suites[suite] ||= RUNS.select { _1["suite"] == suite }.sort_by { [_1["t"], _1["idx"]] }.map do |r|
    r.merge("b" => suite == "iriq" ? iriq_behaviors(r) : behaviors(r))
  end
end

# ------------------------------------------------------------------- rules
# floor_ms: absolute excess over baseline median; ratio: relative excess;
# k: robust z (1.4826*MAD); above_max: slower than every baseline run;
# isolate: suppress when an adjacent example (execution order) also slowed by
# >= isolate x this excess, i.e. a machine stall rather than a code change;
# stall: suppress when the rest of the run (suite excess minus this example's)
# slowed more than this example and by > stall x the suite's robust spread.
# suite: whether suite duration may signal on its own.
LAT = { n_min: 2, floor_ms: 100.0, ratio: 4.0, k: 0.0, above_max: true, isolate: 0.5, stall: 3.0, suite: false }

# Rule of succession: after n runs without an event, P(event) ~ 1/(n+2).
def n_evidence(n) = (n + 1.0) / (n + 2)

def latency_rows(cur, base)
  cur.filter_map do |id, v|
    next unless v["ms"]
    xs = base.filter_map { _1.dig(id, "ms") }
    next if xs.empty?
    { id:, seq: v["seq"], c: v["ms"], n: xs.size, med: median(xs), s: 1.4826 * mad(xs), max: xs.max }
  end
end

def latency(rows, p = LAT)
  excess = rows.select { _1[:seq] }.to_h { [_1[:seq], _1[:c] - _1[:med]] }
  suite = rows.find { _1[:id] == "suite" }
  rows.filter_map do |r|
    next if r[:n] < p[:n_min]
    next if r[:id] == "suite" && !p[:suite]
    need = [p[:floor_ms], (p[:ratio] - 1) * r[:med], p[:k] * r[:s]].max
    delta = r[:c] - r[:med]
    next unless delta > need
    next if p[:above_max] && r[:c] <= r[:max]
    if p[:isolate] && r[:seq]
      neighbor = [excess[r[:seq] - 1], excess[r[:seq] + 1]].compact.max || 0
      next if neighbor >= p[:isolate] * delta
    end
    if p[:stall] && suite && r[:seq]
      rest = (suite[:c] - suite[:med]) - delta
      next if rest > delta && rest > p[:stall] * suite[:s]
    end
    e = delta / need
    { kind: "LATENCY", id: r[:id], conf: sig(n_evidence(r[:n]) * e / (1 + e), 2), from: sig(r[:med]), to: sig(r[:c]), n: r[:n] }
  end
end

def presence(id, cur, base, n_min: 2)
  return if base.size < n_min
  k = base.count { _1.key?(id) }
  n = base.size
  if cur && k.zero?
    { kind: "NEW", id:, conf: sig(n_evidence(n), 2), n:, to: cur["count"] }
  elsif !cur && k == n
    { kind: "DISAPPEARED", id:, conf: sig(n_evidence(n), 2), n:, from: median(base.map { _1[id]["count"] }) }
  end
end

# Exact counts (same in every baseline run) signal on any change; varying ones
# only outside the baseline range by more than twice its width.
def frequency(id, field, cur, base, n_min: 2)
  xs = base.filter_map { _1.dig(id, field) }
  return unless cur && cur[field] && xs.size >= n_min && xs.size == base.size
  c = cur[field]
  lo, hi = xs.minmax
  med = median(xs)
  return if c.between?(lo, hi)
  exact = lo == hi
  return unless exact || (c - med).abs > 2 * (hi - lo)
  conf = n_evidence(xs.size) * (exact ? 1 : 1 - (hi - lo).to_f / (c - med).abs)
  { kind: "FREQUENCY", id:, field:, conf: sig(conf, 2), from: med, to: c, exact:, n: xs.size }
end

def error(id, cur, base, n_min: 1)
  return unless cur && cur["status"] == "failed"
  st = base.filter_map { _1.dig(id, "status") }
  return if st.size < n_min
  j = st.count("failed")
  # Known-flaky with the same exception is not news.
  return if j.positive? && base.any? { _1.dig(id, "exception") == cur["exception"] }
  { kind: "ERROR", id:, conf: sig(1 - (j + 1.0) / (st.size + 2), 2), n: st.size, flaky: j, exception: cur["exception"] }
end

def signals(cur, base, lat: LAT)
  out = latency(latency_rows(cur, base), lat)
  (cur.keys | base.flat_map(&:keys)).each do |id|
    c = cur[id]
    out << presence(id, c, base)
    next unless c
    case c["kind"]
    when "example"
      out << error(id, c, base)
      out << frequency(id, "queries", c, base)
    when "request"
      out << frequency(id, "queries", c, base) << frequency(id, "count", c, base)
    when "sql", "stderr" then out << frequency(id, "count", c, base)
    end
  end
  out.compact
end

# ---------------------------------------------------------------- grouping
# Tier = how directly a signal names a developer-visible change. Lower wins.
def tier(s, b)
  kind = s[:id] == "suite" ? "suite" : (b.dig(s[:id], "kind") || s[:id].split(":").first)
  case [s[:kind], kind]
  in ["ERROR", _] then 1
  in ["FREQUENCY", "request"] | ["NEW", "stderr"] | ["LATENCY", "example"] then 2
  in ["FREQUENCY", "example" | "sql" | "stderr"] then 3
  in [_, "suite"] then 5
  else 4
  end
end

# Example ids a signal belongs to: its own example, or the examples whose
# per-example count of that behavior moved against the baseline median.
def owners(s, cur, base)
  return [s[:id].delete_prefix("example:")] if s[:id].start_with?("example:")
  key = s[:field] == "queries" ? "queries_by_example" : "by_example"
  now = cur.dig(s[:id], key) || {}
  was = base.map { _1.dig(s[:id], key) || {} }
  (now.keys | was.flat_map(&:keys)).select { |ex| now.fetch(ex, 0) != median(was.map { _1.fetch(ex, 0) }) }
end

def groups(sigs, cur, base)
  all = cur.merge(*base.reverse) { |_, a, _| a }
  g = Hash.new { |h, k| h[k] = [] }
  suite = []
  sigs.each do |s|
    s[:tier] = tier(s, all)
    next suite << s if s[:tier] == 5
    own = owners(s, cur, base)
    # Setup-only changes (schema load, fixtures) describe the environment, not
    # the code under test: one low-tier group.
    s[:tier] = 5 if own == [SETUP]
    key = if own.size == 1 then "example:#{own.first}"
          elsif s[:id].start_with?("stderr:") then s[:id][0, 60] # same message, different call site
          else s[:id]
          end
    g[key] << s
  end
  ranked = g.map { |key, ss| [key, ss.sort_by { [_1[:tier], -_1[:conf]] }] }.sort_by { |_, ss| [ss.first[:tier], -ss.first[:conf]] }
  if ranked.empty? then ranked = suite.map { [_1[:id], [_1]] }
  else ranked.first[1].concat(suite)
  end
  ranked
end

def label(s, b)
  name = b.dig(s[:id], "name") || s[:id]
  detail = s.slice(:field, :from, :to, :exception, :flaky).map { |k, v| "#{k}=#{v}" }.join(" ")
  "#{s[:kind]} #{name[0, 110]} #{detail} conf=#{s[:conf]} n=#{s[:n]}"
end

# --------------------------------------------------------------- baselines
# Baseline = the n clean runs nearest before this run (after it when too few
# precede), never the run itself.
def baseline_for(run, clean, n)
  others = clean.reject { _1.equal?(run) }
  before = others.select { ([_1["t"], _1["idx"]] <=> [run["t"], run["idx"]]) < 0 }
  after = others - before
  (before.last(n) + after.first(n - before.last(n).size)).map { _1["b"] }
end

# ----------------------------------------------------------------- reports
def corr(a, b)
  ma, mb = a.sum / a.size, b.sum / b.size
  a.zip(b).sum { (_1 - ma) * (_2 - mb) } / Math.sqrt(a.sum { (_1 - ma)**2 } * b.sum { (_1 - mb)**2 })
end

def noise(suite)
  runs = load_suite(suite).select { _1["scenario"] == "baseline" }
  puts "\n## #{suite}: #{runs.size} clean runs, load1 #{runs.map { _1['load1'] }.minmax.join('..')}"
  d = runs.map { _1["duration"] * 1000 }
  puts "suite duration ms: median #{sig(median(d))} MAD/med #{sig(mad(d) / median(d), 2)} min #{sig(d.min)} max #{sig(d.max)} max/min #{sig(d.max / d.min, 2)}"
  lt = runs.map { _1["load_time"] }
  puts "load_time s: median #{sig(median(lt))} max/min #{sig(lt.max / lt.min, 2)}; wall s: median #{sig(median(runs.map { _1['wall'] }))}"
  puts "corr(load1, suite duration) #{sig(corr(runs.map { _1['load1'] }, d), 2)}"

  ids = runs.first["b"].keys.grep(/\Aexample:/)
  rows = ids.filter_map do |id|
    xs = runs.filter_map { _1.dig("b", id, "ms") }
    next if xs.size < runs.size
    m = median(xs)
    { m:, ratio: xs.max / [xs.min, 0.001].max, madr: mad(xs) / m, dev: xs.max - m, devr: xs.max / m }
  end
  puts "| median bin | examples | max/min p50 | max/min max | MAD/med p50 | worst excess ms p50 | worst excess ms max | worst max/med p50 | worst max/med max |"
  puts "|---|---|---|---|---|---|---|---|---|"
  [[0, 1], [1, 10], [10, 100], [100, 1e9]].each do |lo, hi|
    r = rows.select { _1[:m] >= lo && _1[:m] < hi }
    next if r.empty?
    f = ->(key, p) { sig(p == :max ? r.map { _1[key] }.max : q(r.map { _1[key] }, p), 2) }
    puts "| #{lo}-#{hi == 1e9 ? '' : hi}ms | #{r.size} | #{f.(:ratio, 0.5)} | #{f.(:ratio, :max)} | #{f.(:madr, 0.5)} | #{f.(:dev, 0.5)} | #{f.(:dev, :max)} | #{f.(:devr, 0.5)} | #{f.(:devr, :max)} |"
  end
  top = rows.sort_by { -_1[:dev] }.first(3).map { "+#{sig(_1[:dev], 2)}ms on median #{sig(_1[:m], 2)}ms" }
  puts "largest single excursions: #{top.join(', ')}"
  return if suite == "iriq"

  load_suite(suite).group_by { _1["scenario"] }.each do |scenario, rs|
    count_ids = rs.flat_map { _1["b"].keys }.uniq - ["suite"]
    varying = count_ids.flat_map do |id|
      %w[count queries].filter_map do |field|
        xs = rs.map { _1.dig("b", id, field) }
        next if xs.compact.empty? || xs.uniq.size == 1 || (field == "count" && id.start_with?("example:"))
        "#{id[0, 100]} #{field}: #{xs.tally.map { |k, v| "#{k.inspect}x#{v}" }.join(' ')}"
      end
    end
    puts "#{scenario}: #{rs.size} runs, #{count_ids.size} count behaviors, varying within scenario: #{varying.size}"
    varying.each { puts "  #{_1}" }
  end
end

# Every (suite, scenario, n, run) comparison with its signals and groups.
def comparisons(ns: [2, 3, 5, 10], lat: LAT)
  %w[demo demorand iriq].flat_map do |suite|
    runs = load_suite(suite)
    clean = runs.select { _1["scenario"] == "baseline" }
    runs.flat_map do |r|
      ns.filter_map do |n|
        next if clean.size <= n
        base = baseline_for(r, clean, n)
        sigs = signals(r["b"], base, lat:)
        { suite:, scenario: r["scenario"], n:, run: r, base:, sigs:, groups: groups(sigs, r["b"], base) }
      end
    end
  end
end

EXPECT = {
  "n_plus_one" => ->(h) { h[:kind] == "FREQUENCY" && h[:id] == "request:UsersController#show" },
  "slow" => ->(h) { h[:kind] == "LATENCY" && h[:id].include?("post_spec.rb[1:2]") },
  "warn" => ->(h) { h[:kind] == "NEW" && h[:id].start_with?("stderr:DEPRECATION") },
  "fail" => ->(h) { h[:kind] == "ERROR" && h[:id].include?("user_spec.rb[1:2]") },
}

def backtest
  puts "\n## Backtest with LAT=#{LAT}"
  comparisons.group_by { [_1[:suite], _1[:scenario], _1[:n]] }.each do |(suite, scenario, n), cs|
    b = cs.first[:run]["b"]
    heads = cs.map { _1[:groups].first&.then { |_, ss| ss.first } }
    line = "#{suite}/#{scenario} n=#{n}: comparisons #{cs.size}, with any signal #{cs.count { _1[:sigs].any? }}, signals #{cs.sum { _1[:sigs].size }}, groups #{cs.sum { _1[:groups].size }}"
    line += ", headline correct #{heads.count { _1 && EXPECT[scenario].(_1) }}/#{cs.size}" if EXPECT[scenario]
    puts line
    next if scenario == "baseline" && cs.all? { _1[:sigs].empty? }
    cs.flat_map { |c| c[:groups].first(3).each_with_index.map { |(_, ss), i| "#{i + 1}. #{label(ss.first, b)}#{ss.size > 1 ? " +#{ss.size - 1} supporting: #{ss.drop(1).map { "#{_1[:kind]} #{_1[:id][0, 40]}" }.join('; ')}" : ''}" } }
      .tally.each { |l, k| puts "  #{k}x #{l}" }
  end
end

# LATENCY threshold sweep over precomputed rows (clean LOO + slow toggle).
def sweep
  rows = %w[demo demorand iriq].flat_map do |suite|
    runs = load_suite(suite)
    clean = runs.select { _1["scenario"] == "baseline" }
    runs.select { %w[baseline slow].include?(_1["scenario"]) }.flat_map do |r|
      [2, 3, 5, 10].filter_map { |n| clean.size > n && [suite, r["scenario"], n, latency_rows(r["b"], baseline_for(r, clean, n))] }
    end
  end
  puts "\n## LATENCY sweep: FP = signals on clean LOO comparisons (n in 2,3,5,10); demo #{rows.count { _1[0] == 'demo' && _1[1] == 'baseline' }} comparisons, iriq #{rows.count { _1[0] == 'iriq' }} (934 examples each)"
  puts "| n_min | floor ms | ratio | isolate | stall | FP demo examples | FP iriq examples | FP comparisons | FP suite-duration (if enabled) | slow TP |"
  puts "|---|---|---|---|---|---|---|---|---|---|"
  [2, 3].product([50.0, 100.0, 150.0], [2.0, 3.0, 4.0], [nil, 0.5], [nil, 3.0]).each do |n_min, floor_ms, ratio, isolate, stall|
    p = { n_min:, floor_ms:, ratio:, k: 0.0, above_max: true, isolate:, stall:, suite: true }
    res = rows.map { |suite, scen, _n, rs| [suite, scen, latency(rs, p)] }
    clean = res.select { _1[1] == "baseline" }
    fp = ->(s) { clean.select { _1[0].start_with?(s) }.sum { |_, _, ss| ss.count { _1[:id] != "suite" } } }
    fpc = clean.count { |_, _, ss| ss.any? { _1[:id] != "suite" } }
    fps = clean.sum { |_, _, ss| ss.count { _1[:id] == "suite" } }
    slow = res.select { _1[1] == "slow" }
    tp = slow.count { |_, _, ss| ss.any? { _1[:id].include?("post_spec.rb[1:2]") } }
    puts "| #{n_min} | #{floor_ms.to_i} | #{ratio} | #{isolate || '-'} | #{stall || '-'} | #{fp.('demo')} | #{fp.('iriq')} | #{fpc}/#{clean.size} | #{fps} | #{tp}/#{slow.size} |"
  end
end

# Recall for a synthetic slowdown of one example: +d ms added to each example
# of each clean run in turn (neighbors untouched), baseline n=5.
def inject(n: 5)
  puts "\n## Synthetic single-example slowdown recall (baseline n=#{n}, LAT=#{LAT})"
  bins = [[0, 1], [1, 10], [10, 100], [100, 1e9]]
  puts "| suite | +d ms | #{bins.map { |lo, hi| "median #{lo}-#{hi == 1e9 ? '' : hi}ms" }.join(' | ')} | all | median conf |"
  puts "|---|---|#{'---|' * bins.size}---|---|"
  %w[demo iriq].each do |suite|
    runs = load_suite(suite)
    clean = runs.select { _1["scenario"] == "baseline" }
    prepared = clean.map { |r| latency_rows(r["b"], baseline_for(r, clean, n)) }
    [10, 25, 50, 100, 300, 1000].each do |d|
      hit = Hash.new(0)
      tot = Hash.new(0)
      confs = []
      prepared.each do |rows|
        rows.each_with_index do |row, i|
          next unless row[:seq]
          bin = bins.index { |lo, hi| row[:med] >= lo && row[:med] < hi }
          tot[bin] += 1
          trial = rows.dup
          trial[i] = row.merge(c: row[:c] + d)
          s = latency([trial[i]] + neighbors(trial, row[:seq]), LAT).find { _1[:id] == row[:id] }
          next unless s
          hit[bin] += 1
          confs << s[:conf]
        end
      end
      cells = bins.each_index.map { |bi| tot[bi].zero? ? "-" : "#{sig(hit[bi].to_f / tot[bi], 2)} (#{tot[bi]})" }
      puts "| #{suite} | #{d} | #{cells.join(' | ')} | #{sig(hit.values.sum.to_f / tot.values.sum, 2)} | #{confs.empty? ? '-' : sig(median(confs), 2)} |"
    end
  end
end

def neighbors(rows, seq)
  @by_seq ||= {}
  rows.select { [seq - 1, seq + 1].include?(_1[:seq]) }
end

mode = ARGV.fetch(0, "all")
%w[demo demorand iriq].each { noise(_1) } if %w[noise all].include?(mode)
sweep if %w[sweep all].include?(mode)
backtest if %w[backtest all].include?(mode)
inject if %w[inject all].include?(mode)
