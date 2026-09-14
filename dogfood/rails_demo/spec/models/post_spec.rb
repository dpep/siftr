require "rails_helper"

RSpec.describe Post do
  let(:user) { User.create!(name: "Ada", email: "ada@example.com") }

  it "requires a title" do
    expect(user.posts.new).not_to be_valid
  end

  it "summarizes the body" do
    post = user.posts.new(title: "Hi", body: "word " * 20)
    expect(post.summary.length).to be <= 40
  end
end
