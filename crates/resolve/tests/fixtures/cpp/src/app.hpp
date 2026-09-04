#ifndef APP_HPP
#define APP_HPP

int free_helper(int n);

class Foo {
public:
    void bar();
    void close();
    ~Foo() { close(); }
    int count;
};

class Other {
public:
    void bar() { }
    void close() { }
};

#endif
