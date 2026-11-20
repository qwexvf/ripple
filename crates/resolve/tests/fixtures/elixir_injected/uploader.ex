defmodule App.Uploader do
  use Waffle.Definition

  def storage_dir(_version, _scope), do: "uploads"
end
