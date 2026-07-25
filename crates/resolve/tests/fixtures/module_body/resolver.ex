defmodule App.Resolvers.PlayerResolver do
  def me(_, _, _), do: :ok
  def follow(_, _, _), do: :ok
  def legacy(_, _, _), do: :ok
end
