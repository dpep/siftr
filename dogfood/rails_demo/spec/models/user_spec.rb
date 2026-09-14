require "rails_helper"

RSpec.describe User do
  it "requires a name" do
    expect(User.new(email: "a@example.com")).not_to be_valid
  end

  it "requires an email" do
    expect(User.new(name: "Ada")).not_to be_valid
  end

  it "has a display name" do
    expect(User.new(name: "Ada").display_name).to eq("Ada")
  end

  it "destroys posts with the user" do
    user = User.create!(name: "Ada", email: "ada@example.com")
    user.posts.create!(title: "Hello")
    expect { user.destroy }.to change(Post, :count).by(-1)
  end
end
