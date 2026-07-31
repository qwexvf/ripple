def helper(x):
    return x + 1


class Client:
    def send(self, body):
        return helper(body)


class Server:
    def send(self, body):
        return body
