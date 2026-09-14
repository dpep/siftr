#!/usr/bin/env ruby
# Reduces raw run dirs from collect.sh into the small committed data set:
#   data/runs.json         per run: meta, per-example status/run_time, and for
#                          the demo per-example SQL template counts + stderr templates
#   data/iriq_run_time.csv example x run matrix of run_time in microseconds
# usage: extract.rb <raw_dir>
require "json"
require "csv"

RAW = ARGV.fetch(0)
DATA = File.join(__dir__, "data")
Dir.mkdir(DATA) unless Dir.exist?(DATA)

ANSI = /\e\[[0-9;]*m/
# "  User Load (0.1ms)  SELECT ...  [[binds]]"
SQL = /\A\s+(?<name>[A-Z][\w:]*(?: [\w:]+)*) \((?<ms>[\d.]+)ms\)\s+(?<sql>.*?)(?:\s\s\[\[.*)?\z/

# ActionView compiled-template method names embed a per-boot String#hash with
# "-" rewritten to "_" (erb__123 vs erb___123), so a digits-only mask splits one
# warning into two templates. Collapse the underscore run too.
def template(s)
  s.gsub(/_+\d+/, "_N").gsub(/\d+(\.\d+)?/, "N").gsub(%r{/\S+}, "PATH")
end

def naive_template(s) = s.gsub(/\d+(\.\d+)?/, "N").gsub(%r{/\S+}, "PATH")

# SQL templates are stored once in runs.json and referenced by index.
TEMPLATES = {}

def sql_counts(slice)
  counts = Hash.new(0)
  slice.each_line do |line|
    m = SQL.match(line.gsub(ANSI, "").chomp) or next
    t = "#{m[:name]}: #{template(m[:sql])}"
    counts[(TEMPLATES[t] ||= TEMPLATES.size).to_s] += 1
  end
  counts
end

# Rails 8.1 "Completed ... (N queries, M cached)" per controller action.
def request_queries(slice)
  out = {}
  action = nil
  slice.each_line do |line|
    line = line.gsub(ANSI, "")
    if (m = line.match(/\AProcessing by (\S+) as/)) then action = m[1]
    elsif action && (m = line.match(/\ACompleted .*\((\d+) queries, (\d+) cached\)/))
      r = (out[action] ||= { count: 0, queries: 0 })
      r[:count] += 1
      r[:queries] += m[1].to_i
      action = nil
    end
  end
  out
end

runs = []
iriq = {}
Dir.glob(File.join(RAW, "*/*/*")).sort.each do |dir|
  suite, scenario, idx = dir.split("/").last(3)
  next unless File.exist?(File.join(dir, "exit_code.txt")) # run still in progress
  events = File.readlines(File.join(dir, "rspec.ndjson")).map { JSON.parse(_1) }
  summary = events.find { _1["event"] == "summary" } || {}
  log = File.exist?(File.join(dir, "test.log")) ? File.binread(File.join(dir, "test.log")) : nil
  starts = events.select { _1["event"] == "example_started" }.to_h { [_1["id"], _1["log_offset"]] }

  run = {
    suite:, scenario:, idx: idx.to_i, t: File.mtime(File.join(dir, "load.txt")).to_i,
    load1: File.read(File.join(dir, "load.txt")).to_f,
    wall: File.read(File.join(dir, "wall.txt")).to_f.round(3),
    exit: File.read(File.join(dir, "exit_code.txt")).to_i,
    duration: summary["duration"]&.round(4), load_time: summary["load_time"]&.round(3),
    stderr: File.readlines(File.join(dir, "stderr.txt")).map { template(_1.chomp) }.tally,
    stderr_naive: File.readlines(File.join(dir, "stderr.txt")).map { naive_template(_1.chomp) }.tally,
  }

  examples = events.select { _1["event"] == "example" }
  if suite == "iriq"
    iriq[[scenario, idx]] = examples.to_h { [_1["id"], (_1["run_time"] * 1e6).round] }
  else
    first = starts.values.min
    run[:suite_sql] = sql_counts(log.byteslice(0, first)) if log && first
    run[:examples] = examples.each_with_index.map do |e, seq|
      h = { id: e["id"], seq:, name: e["full_description"], status: e["status"], run_time: e["run_time"] }
      h[:exception] = e.dig("exception", "class") if e["exception"]
      if log
        slice = log.byteslice(starts[e["id"]], e["log_offset"] - starts[e["id"]])
        h[:sql] = sql_counts(slice)
        h[:requests] = request_queries(slice)
      end
      h
    end
  end
  runs << run
end

File.write(File.join(DATA, "runs.json"), JSON.generate({ templates: TEMPLATES.keys, runs: }) + "\n")

unless iriq.empty?
  keys = iriq.keys.sort
  ids = keys.flat_map { iriq[_1].keys }.uniq
  CSV.open(File.join(DATA, "iriq_run_time.csv"), "w") do |csv|
    csv << ["id", *keys.map { _1.join("/") }]
    ids.each { |id| csv << [id, *keys.map { iriq[_1][id] }] }
  end
end
warn "#{runs.size} runs, #{iriq.size} iriq runs"
