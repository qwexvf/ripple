package a.b

object C {
  def helper(n: Int): Int = n + 1
}

class Box {
  def send(n: Int): Int = trim(n)
  def trim(n: Int): Int = n
}

class Rival {
  def send(n: Int): Int = n
  def trim(n: Int): Int = -n
}

def free(n: Int): Int = n + 2
