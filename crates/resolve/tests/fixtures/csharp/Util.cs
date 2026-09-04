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
            return Helper(n) + Trim(n);
        }

        public int Trim(int n)
        {
            return n;
        }
    }

    public class Rival
    {
        public int Send(int n)
        {
            return n;
        }

        public int Trim(int n)
        {
            return -n;
        }
    }
}
