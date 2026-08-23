class Client:
    def get(self):
        return self.request()

    def request(self):
        return 1


class AsyncClient:
    def request(self):
        return 2
