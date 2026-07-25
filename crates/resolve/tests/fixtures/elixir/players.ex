defmodule App.Players do
  alias App.Repo

  # a typespec names a type, not a call site
  @spec get_player(String.t()) :: map()
  def get_player(id) do
    id
    |> normalize()
    |> fetch()
  end

  def list_players(filters) do
    filters |> normalize() |> Repo.all()
  end

  # multi-clause: clauses must not link to each other
  def kind(:admin), do: :admin
  def kind(_other), do: :user

  # self-recursion adds nothing to a blast radius
  def countdown(0), do: :done
  def countdown(n), do: countdown(n - 1)

  @type normalize_result :: normalize(term())
  defp normalize(x), do: x

  defp fetch(id) do
    Repo.get(Player, id)
  end
end
