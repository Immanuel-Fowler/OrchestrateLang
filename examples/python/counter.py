from orchestratelang import landline


class Counter(landline.Serverlet):
    def __init__(self):
        self.total = 0

    def add(self, amount: int) -> int:
        self.total += amount
        return self.total


landline.serve(Counter)
