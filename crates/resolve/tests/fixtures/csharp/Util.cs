using System.Text;

namespace Demo
{
    public class Util
    {
        public static int Helper(int n)
        {
            return n + 1;
        }

        public int Send(int n)
        {
            return Helper(n);
        }
    }
}
