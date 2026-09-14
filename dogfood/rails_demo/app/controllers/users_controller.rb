class UsersController < ApplicationController
  def index
    @users = User.order(:name)
  end

  def show
    @user = User.find(params[:id])
    # SIFTR_DEMO_N_PLUS_ONE drops the preload: one comments query per post.
    @posts = ENV["SIFTR_DEMO_N_PLUS_ONE"] ? @user.posts : @user.posts.includes(:comments)
  end
end
