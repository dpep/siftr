module RailsDemo
  def self.deprecator
    @deprecator ||= ActiveSupport::Deprecation.new("2.0", "RailsDemo")
  end
end

# Registering makes it follow config.active_support.deprecation, like Rails' own.
Rails.application.deprecators[:rails_demo] = RailsDemo.deprecator
