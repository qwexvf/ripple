defmodule Web.Player.Queries do
  use Absinthe.Schema.Notation
  alias App.Resolvers.PlayerResolver

  object :player_queries do
    field :current_player, :player do
      resolve(&PlayerResolver.me/3)
    end

    field :duplicated, :player, resolve: &PlayerResolver.me/3
  end

  # two imported objects declaring the same root field → ambiguous
  object :legacy_queries do
    field :duplicated, :player, resolve: &PlayerResolver.legacy/3
  end

  object :player_mutations do
    field :follow_player, :player, resolve: &PlayerResolver.follow/3
  end

  # a field on a type, not a root field: same name, different resolver
  object :player do
    field :current_player, :player, resolve: &PlayerResolver.decoy/3
  end
end
