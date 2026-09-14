require "rails_helper"

RSpec.describe "Users", type: :request do
  let!(:user) do
    User.create!(name: "Ada", email: "ada@example.com").tap do |u|
      8.times do |i|
        post = u.posts.create!(title: "Post #{i}")
        2.times { |j| post.comments.create!(body: "Comment #{i}.#{j}") }
      end
    end
  end

  it "lists users" do
    get users_path
    expect(response).to have_http_status(:ok)
    expect(response.body).to include("Ada")
  end

  it "shows a user with posts and comments" do
    get user_path(user)
    expect(response).to have_http_status(:ok)
    expect(response.body).to include("Post 7", "Comment 7.1")
  end
end
