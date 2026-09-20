#!/usr/bin/env ruby
# Reduces a directory of per-run `changes -j` dumps to the false-positive floor.
#
#   tally.rb <dir> [<dir> ...]
#
# Run the corpus through a clean context only: nothing changed between its runs,
# so every signal counted here is by construction a false positive. A comparison
# counts only once siftr has n >= 2 baseline runs, since every rule declines
# below that; runs with fewer are reported separately rather than folded in as
# free zeroes. `still open` is counted too — a re-shown signal is noise the
# developer reads even though it is not a new claim.
require "json"

def tally(dir)
  files = Dir[File.join(dir, "*.json")].sort
  abort "no json in #{dir}" if files.empty?

  judged = 0
  below_n = 0
  dirty = Hash.new(0)   # comparisons carrying >= 1 signal of this kind
  counts = Hash.new(0)  # signals of this kind
  by_n = Hash.new { |h, k| h[k] = [0, 0] } # baseline size => [judged, dirty]
  non_example_latency = 0
  still_open = 0
  behaviors = []
  detail = []

  files.each do |f|
    d = JSON.parse(File.read(f))
    n = (d["baseline_runs"] || []).size
    if n < 2
      below_n += 1
      next
    end
    judged += 1
    behaviors << d["behaviors"]
    still_open += (d["open_signals"] || []).size
    signals = d["signals"] || []
    by_n[n][0] += 1
    next if signals.empty?

    by_n[n][1] += 1
    signals.group_by { |s| s["kind"] }.each do |kind, group|
      dirty[kind] += 1
      counts[kind] += group.size
    end
    non_example_latency += signals.count do |s|
      s["kind"] == "latency" && s.dig("behavior", "kind") != "test.example"
    end
    detail << [File.basename(f, ".json"),
               signals.map { |s| "#{s["kind"]}/#{s.dig("behavior", "kind")}" }.tally]
  end

  puts dir
  puts "  comparisons judged (n>=2 baseline): #{judged}   below n=2, not judged: #{below_n}"
  puts "  behaviors per run: #{behaviors.min}-#{behaviors.max}"
  puts "  comparisons with >= 1 signal: #{detail.size} of #{judged}"
  if counts.empty?
    puts "  signals: none, in any kind"
  else
    counts.sort.each { |k, n| puts "  #{k.upcase}: #{n} signals in #{dirty[k]} comparisons" }
  end
  puts "  LATENCY on a non-example behavior: #{non_example_latency}"
  puts "  still-open signals re-shown, summed over comparisons: #{still_open}"
  puts "  by baseline size: " + by_n.sort.map { |n, (j, bad)| "n=#{n}: #{bad}/#{j}" }.join("  ")
  detail.each { |run, kinds| puts "    #{run}: #{kinds.inspect}" }
end

abort "usage: tally.rb <dir> [<dir> ...]" if ARGV.empty?
ARGV.each { |d| tally(d) }
