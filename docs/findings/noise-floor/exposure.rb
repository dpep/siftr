#!/usr/bin/env ruby
# What a corpus could have fired, from the `summary.json` replay.sh/suite.sh
# leave behind. A floor of zero is only worth the exposure behind it: a kind
# with no candidate behavior in the corpus could not have fired, and its zero
# is arithmetic, not evidence.
#
#   exposure.rb <out_dir> [<out_dir> ...]
require "json"

ARGV.each do |dir|
  f = File.join(dir, "summary.json")
  next warn("no summary.json in #{dir}") unless File.exist?(f)

  behaviors = JSON.parse(File.read(f))["behaviors"]
  by_kind = behaviors.group_by { |b| b["behavior"]["kind"] }
  timed = behaviors.count { |b| b.dig("stats", "duration") }

  puts dir
  puts "  behaviors: #{behaviors.size}"
  by_kind.sort_by { |k, v| [-v.size, k.to_s] }.each do |kind, group|
    t = group.count { |b| b.dig("stats", "duration") }
    puts "    #{kind}: #{group.size}  (#{t} timed)"
  end
  puts "  LATENCY candidates: #{timed} timed behaviors, " \
       "#{by_kind.fetch("test.example", []).count { |b| b.dig("stats", "duration") }} of them examples"
  puts "  ERROR candidates: #{by_kind.fetch("test.example", []).size} examples"
  puts "  NEW/DISAPPEARED/FREQUENCY candidates: all #{behaviors.size}"
end
