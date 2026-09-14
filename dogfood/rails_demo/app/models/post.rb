class Post < ApplicationRecord
  belongs_to :user
  has_many :comments, dependent: :destroy

  validates :title, presence: true

  def summary
    sleep 0.3 if ENV["SIFTR_DEMO_SLOW"]
    body.to_s.truncate(40)
  end
end
