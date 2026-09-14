require "rails_helper"

RSpec.describe Comment do
  it "requires a body" do
    expect(Comment.new).not_to be_valid
  end

  it "is hidden when flagged" do
    pending "moderation is not built yet"
    expect(Comment.new).to respond_to(:hidden?)
  end
end
