defmodule App.Notes do
  alias App.Uploader

  def remove(note) do
    Uploader.delete({note.image, note})
  end

  def dir do
    Uploader.storage_dir(:original, nil)
  end

  def elsewhere do
    Enum.map([1], & &1)
  end
end
