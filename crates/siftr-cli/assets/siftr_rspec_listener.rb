# Loaded by `siftr run` via SPEC_OPTS="--require <this file>". Streams one JSON object per
# RSpec event to $SIFTR_RSPEC_EVENTS. A listener, not a formatter: adding a formatter
# would suppress RSpec's default progress output.
#
# $SIFTR_RSPEC_LOG (optional, e.g. log/test.log): every event is stamped with that file's
# size and inode as the trailing keys "log_offset","log_ino". siftr rebases them onto the
# run's log slice (surviving a rotation) and drops them where it can't place them.
# Relies on the app logger flushing each line (Rails does).
require "json"

module SiftrRspec
  class Listener
    EVENTS = %i[start example_started example_passed example_failed example_pending dump_summary].freeze

    def initialize(path, log_path)
      @io = File.open(path, "a")
      @io.sync = true
      @log_path = log_path
    end

    def start(n)
      emit(event: "start", expected: n.count, load_time: n.load_time)
    end

    def example_started(n)
      emit(event: "example_started", id: n.example.id)
    end

    def example_passed(n) = example(n.example)
    def example_failed(n) = example(n.example)
    def example_pending(n) = example(n.example)

    def dump_summary(n)
      emit(event: "summary", duration: n.duration, load_time: n.load_time,
           examples: n.example_count, failures: n.failure_count, pending: n.pending_count,
           errors_outside_of_examples: n.errors_outside_of_examples_count)
    end

    private

    def example(ex)
      r = ex.execution_result
      h = { event: "example", id: ex.id, description: ex.description,
            full_description: ex.full_description, file_path: ex.metadata[:file_path],
            line_number: ex.metadata[:line_number], status: r.status.to_s, run_time: r.run_time }
      h[:pending_message] = r.pending_message if r.pending_message
      if (e = r.exception) && r.status == :failed
        h[:exception] = { class: e.class.name, message: e.message.to_s[0, 2000] }
      end
      emit(h)
    end

    # log_offset and log_ino must stay the last keys: siftr rewrites that suffix.
    def emit(h)
      if @log_path && (stat = (File.stat(@log_path) rescue nil))
        h[:log_offset] = stat.size
        h[:log_ino] = stat.ino
      end
      @io.write(JSON.generate(h) << "\n")
    end
  end
end

if (path = ENV["SIFTR_RSPEC_EVENTS"]) && !path.empty?
  log_path = ENV["SIFTR_RSPEC_LOG"]
  log_path = nil if log_path&.empty?
  RSpec.configure do |c|
    c.reporter.register_listener(SiftrRspec::Listener.new(path, log_path), *SiftrRspec::Listener::EVENTS)
  end
end
