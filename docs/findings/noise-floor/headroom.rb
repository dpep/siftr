#!/usr/bin/env ruby
# How far a clean corpus's own noise sits from the LATENCY threshold, per
# behavior, from siftr's stored aggregates.
#
#   headroom.rb <siftr> <siftr_home> <run_count>
#
# A floor of zero says the rule did not fire; this says by how much. The rule
# needs Delta = mean - median(baseline) > max(100ms, 3*median), so the headroom
# is the worst observed move against that need. A small multiple means the zero
# is a near miss and the next machine could turn it over; a large one means the
# threshold is nowhere near this population.
require "json"

siftr, home, runs = ARGV[0], ARGV[1], Integer(ARGV[2])
abort "usage: headroom.rb <siftr> <siftr_home> <run_count>" unless siftr && home

FLOOR_MS = 100.0
RATIO = 4.0 # need = max(FLOOR, (RATIO - 1) * median)

means = Hash.new { |h, k| h[k] = [] } # [kind, template] => mean ms per run
1.upto(runs) do |i|
  out = `SIFTR_HOME=#{home} #{siftr} summary -j -n 100000 r#{i} 2>/dev/null`
  next if out.strip.empty?

  JSON.parse(out)["behaviors"].each do |b|
    d = b.dig("stats", "duration") or next
    means[[b["behavior"]["kind"], b["behavior"]["template"].to_s[0, 48]]] <<
      d["total_us"] / 1000.0 / d["count"]
  end
end

puts format("%-13s %-50s %5s %8s %8s %8s %7s", "kind", "template", "runs", "median", "worst+", "need", "need/+")
means.sort_by { |(kind, _), _| kind }.each do |(kind, template), xs|
  srt = xs.sort
  median = srt[srt.size / 2]
  worst = xs.map { |x| x - median }.max
  need = [FLOOR_MS, (RATIO - 1) * median].max
  puts format("%-13s %-50s %5d %8.3f %8.3f %8.1f %7.0fx",
              kind, template, xs.size, median, worst, need, worst.zero? ? Float::INFINITY : need / worst)
end
