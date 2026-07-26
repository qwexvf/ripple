defmodule Web.Schema do
  use Absinthe.Schema

  query do
    import_fields(:player_queries)
  end
end
