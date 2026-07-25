defmodule App.Boot do
  alias App.Resolvers.PlayerResolver

  # a call in the module body: real, but inside no function
  PlayerResolver.me(nil, nil, nil)

  def start do
    PlayerResolver.follow(nil, nil, nil)
  end
end
