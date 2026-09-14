from orchestratelang import landline


class Counter(landline.Serverlet):
    def __init__(self):
        self.count = 0

    def tick(self) -> int:
        self.count += 1
        return self.host.world.record(self.count)


landline.serve(Counter)
