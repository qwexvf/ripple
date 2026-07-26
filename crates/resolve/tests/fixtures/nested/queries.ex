defmodule Web.Player.Queries do
  use Absinthe.Schema.Notation
  alias App.Resolvers.PlayerResolver

  object :player_queries do
    # the root field declares the type its children are declared in
    field :current_player, :player, resolve: &PlayerResolver.me/3
  end

  # a field on a type, not a root field: reachable only by descending
  object :player do
    field :team, list_of(:team), resolve: &PlayerResolver.team_of/3
    # served by a context module, with no function named anywhere
    field :badges, list_of(:badge), resolve: dataloader(App.Badges)
  end
end
