package x

import a.b.C
import a.b.Box

object Main {
  def run(n: Int): Int = C.helper(n)
  def viaParam(b: Box, n: Int): Int = b.send(n)
  def viaVal(n: Int): Int = {
    val b: Box = mk()
    b.send(n)
  }
}
