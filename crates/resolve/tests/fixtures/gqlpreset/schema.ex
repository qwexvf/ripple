defmodule Web.Schema do
  use Absinthe.Schema

  query do
    import_fields(:player_queries)
    import_fields(:legacy_queries)
  end

  mutation do
    import_fields(:player_mutations)
  end
end
