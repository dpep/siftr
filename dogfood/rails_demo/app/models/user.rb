class User < ApplicationRecord
  has_many :posts, dependent: :destroy

  validates :name, presence: true
  # SIFTR_DEMO_FAIL drops a validation, so the model spec that relies on it fails.
  validates :email, presence: true unless ENV["SIFTR_DEMO_FAIL"]

  def display_name
    RailsDemo.deprecator.warn("User#display_name is deprecated; use #name") if ENV["SIFTR_DEMO_WARN"]
    name
  end
end
