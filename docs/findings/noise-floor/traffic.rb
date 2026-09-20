# Run under the dogfood app: bin/rails runner traffic.rb <out_dir> <batches> <per_batch>
#
# Dispatches identical batches of requests through the booted development stack
# and slices log/development.log per batch, so each <out_dir>/<nnn>/test.log is
# what `sources::rails_log` would have handed the analyzer for that batch.
#
# The app boots once. Everything from the router down — controller, Active
# Record, the view, the logger — is the real thing running repeatedly against a
# real SQLite file, so the run-to-run spread in these slices is real. What is
# absent is the socket and the web server: no dogfood server gem is in the
# Gemfile, and the Rails log is what siftr reads either way.
#
# `gap` is not a politeness knob. 52 batches fired back to back finish in under
# four seconds and sample one machine state; the run-to-run spread in them is
# whatever varies within a single breath. Spacing them lets the corpus cross the
# slow episodes — another process's burst, a GC pause, a thermal step — that a
# real context accumulates its runs across, and those are where a false positive
# would come from.
out, batches, per = ARGV[0], Integer(ARGV[1]), Integer(ARGV[2] || 8)
gap = Float(ARGV[3] || 0)
abort "usage: traffic.rb <out_dir> <batches> [per_batch] [gap_s]" unless out

if User.count.zero?
  3.times do |u|
    user = User.create!(name: "User #{u}", email: "user#{u}@example.test")
    3.times { |p| user.posts.create!(title: "Post #{p}", body: "body").comments.create!(body: "c") }
  end
end

session = ActionDispatch::Integration::Session.new(Rails.application)
session.host = "localhost" # dev host authorization rejects the default www.example.com
paths = ["/users", "/users/#{User.first.id}"]
log = Rails.root.join("log/development.log")

fire = -> { per.times { paths.each { |p| session.get(p) } } }

# Boot effects — view compilation, schema reflection, the query cache warming —
# are one-time and would read as a NEW/LATENCY storm in batch 1.
5.times { fire.call }

1.upto(batches) do |b|
  sleep gap if gap.positive? && b > 1
  dir = File.join(out, format("%03d", b))
  FileUtils.mkdir_p(dir)
  File.write(File.join(dir, "load.txt"), `sysctl -n vm.loadavg`.split[1])
  start = File.size(log)
  fire.call
  Rails.logger.flush if Rails.logger.respond_to?(:flush)
  slice = File.open(log, "rb") { |f| f.seek(start); f.read }
  File.binwrite(File.join(dir, "test.log"), slice.to_s)
  warn "batch #{b} bytes=#{slice.to_s.bytesize}"
end
