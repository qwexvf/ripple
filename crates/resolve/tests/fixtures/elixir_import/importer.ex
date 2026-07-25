defmodule App.Importer do
  import App.Helpers

  def run(x) do
    # bare call that crosses a module boundary only because of the import
    helper_fun(x)
  end

  def prefers_local(x) do
    shadowed(x)
  end

  # a local definition of the same name must win over the imported one
  defp shadowed(x), do: {:local, x}
end
