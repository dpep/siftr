# Fault injection for `script/fault-matrix`, loaded into rails_demo's rspec run via
# `SPEC_OPTS="--require …"`. Inert unless an env var below is set, so the same file is
# required by baseline and fault runs alike — the demo's own source stays untouched.
#
#   SIFTR_FAULT_SLEEP_MS  Post#summary sleeps this long (latency fault)
#   SIFTR_FAULT_QUERIES   UsersController#show makes this many extra queries (frequency fault)
#
# Patching in `before(:suite)` because at --require time the app is not loaded yet.

sleep_ms = ENV["SIFTR_FAULT_SLEEP_MS"].to_i
queries = ENV["SIFTR_FAULT_QUERIES"].to_i

RSpec.configure do |config|
  config.before(:suite) do
    if sleep_ms.positive?
      Post.prepend(Module.new do
        define_method(:summary) do
          sleep(sleep_ms / 1000.0)
          super()
        end
      end)
    end

    if queries.positive?
      UsersController.prepend(Module.new do
        define_method(:show) do
          queries.times { User.count }
          super()
        end
      end)
    end
  end
end
