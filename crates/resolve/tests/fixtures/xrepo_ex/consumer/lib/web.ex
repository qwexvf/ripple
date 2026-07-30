defmodule MyApp.Web do
  alias MyApp.Accounts

  def show(id) do
    Accounts.fetch(id)
  end
end
