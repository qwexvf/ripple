defmodule App.BootTest do
  use ExUnit.Case
  alias App.Resolvers.PlayerResolver

  # `test` is a macro, not a definition, so this call has no enclosing function
  test "the resolver is reachable" do
    PlayerResolver.legacy(nil, nil, nil)
  end
end
