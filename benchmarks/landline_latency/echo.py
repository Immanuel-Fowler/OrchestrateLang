from orchestratelang import landline


class PyEcho(landline.Serverlet):
    def number(self, n: int) -> int:
        return n

    def text(self, s: str) -> str:
        return s

    def numbers(self, items: list[int]) -> list[int]:
        return items

    def floats(self, items: list[float]) -> list[float]:
        return items


landline.serve(PyEcho)
