using System;
using Demo;

namespace Other
{
    public class App
    {
        private Util util;

        public int Run(int n)
        {
            return Util.Helper(n);
        }

        public int ViaParam(Util u, int n)
        {
            return u.Send(n);
        }

        public int ViaField(int n)
        {
            return util.Send(n);
        }

        public int ViaVar(int n)
        {
            var local = new Util();
            return local.Send(n);
        }
    }
}
