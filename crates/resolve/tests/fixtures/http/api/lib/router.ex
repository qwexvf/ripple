defmodule AppWeb.Router do
  scope "/api", AppWeb do
    scope "/v1" do
      get "/users/:id", UserController, :show
      post "/users", UserController, :create
    end
  end
end
